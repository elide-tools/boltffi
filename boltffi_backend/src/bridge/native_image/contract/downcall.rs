//! One `@CFunction` declaration and the Java wrapper that calls it.
//!
//! A downcall pairs a C bridge function with its Native Image spelling. When
//! the C signature carries by-value structs, the downcall targets a
//! `boltffi_ni_*` shim with pointers instead, and the wrapper allocates the
//! storage. The wrapper keeps the exact Java signature the JNI `Native` class
//! exposes so host renderers do not change between bindings.

use crate::{
    bridge::{
        c::{self, Identifier},
        jni::{NativeMethod, NativeParameterKind, NativeReturn},
    },
    core::{Error, Result},
};

use super::{super::NATIVE_IMAGE_BRIDGE, JavaCType};

/// Thread state transition performed around a `@CFunction` call.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Transition {
    /// The thread stays in Java state; the callee must be a short leaf that never re-enters Java.
    NoTransition,
    /// The thread transitions to native state; safe for blocking calls and upcalls.
    ToNative,
}

/// One parameter of a `@CFunction` declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct LibParameter {
    name: Identifier,
    ty: JavaCType,
    c_type: c::Type,
}

impl LibParameter {
    /// Returns the parameter name.
    pub fn name(&self) -> &Identifier {
        &self.name
    }

    /// Returns the Java `@CFunction` type.
    pub const fn ty(&self) -> JavaCType {
        self.ty
    }

    /// Returns the C type the shim or C function receives.
    pub fn c_type(&self) -> &c::Type {
        &self.c_type
    }

    /// Returns the Java parameter declaration.
    pub fn declaration(&self) -> String {
        format!("{} {}", self.ty.java(), self.name.as_str())
    }
}

/// One Java wrapper parameter with the JNI-compatible spelling.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct WrapperParameter {
    name: String,
    java_type: String,
}

impl WrapperParameter {
    /// Returns the Java parameter name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the Java parameter type.
    pub fn java_type(&self) -> &str {
        &self.java_type
    }

    /// Returns the Java parameter declaration.
    pub fn declaration(&self) -> String {
        format!("{} {}", self.java_type, self.name)
    }
}

/// How the Java wrapper body marshals its arguments and result.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WrapperBody {
    /// Every parameter is a scalar and the return is void or a scalar.
    Scalar,
}

/// A `@CFunction` declaration plus its Java wrapper.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Downcall {
    lib_name: Identifier,
    c_function: c::Function,
    needs_shim: bool,
    transition: Transition,
    lib_params: Vec<LibParameter>,
    lib_returns: JavaCType,
    wrapper_params: Vec<WrapperParameter>,
    wrapper_returns: String,
    body: WrapperBody,
}

impl Downcall {
    /// Builds a downcall from one JNI-contract native method.
    pub fn from_native_method(method: &NativeMethod, transition: Transition) -> Result<Self> {
        let c_function = method.c_function().clone();
        let needs_shim = c_function
            .params()
            .iter()
            .any(|parameter| JavaCType::is_by_value_struct(parameter.ty()))
            || JavaCType::is_by_value_struct(c_function.returns());
        let lib_name = match needs_shim {
            true => Identifier::parse(format!("boltffi_ni_{}", c_function.name()))?,
            false => Identifier::parse(c_function.name())?,
        };
        let mut lib_params = c_function
            .params()
            .iter()
            .map(|parameter| {
                let (ty, c_type) = match JavaCType::is_by_value_struct(parameter.ty()) {
                    true => (
                        JavaCType::Pointer,
                        c::Type::ConstPointer(Box::new(parameter.ty().clone())),
                    ),
                    false => (JavaCType::from_c(parameter.ty())?, parameter.ty().clone()),
                };
                Ok(LibParameter {
                    name: Identifier::parse(parameter.name())?,
                    ty,
                    c_type,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let lib_returns = match c_function.returns() {
            c::Type::Status => JavaCType::Int,
            returns if JavaCType::is_by_value_struct(returns) => {
                lib_params.push(LibParameter {
                    name: Identifier::parse("__out")?,
                    ty: JavaCType::Pointer,
                    c_type: c::Type::MutPointer(Box::new(returns.clone())),
                });
                JavaCType::Void
            }
            returns => JavaCType::from_c(returns)?,
        };
        let wrapper_params = method
            .parameters()
            .iter()
            .map(|parameter| match parameter.kind() {
                NativeParameterKind::Scalar(scalar) => Ok(WrapperParameter {
                    name: parameter.name().as_str().to_owned(),
                    java_type: JavaCType::from_jni(scalar.ty()).java().to_owned(),
                }),
                NativeParameterKind::Bytes(_) => Err(Self::unsupported("byte slice parameter")),
                NativeParameterKind::DirectVector(_) => {
                    Err(Self::unsupported("direct vector parameter"))
                }
                NativeParameterKind::Record(_) => Err(Self::unsupported("direct record parameter")),
                NativeParameterKind::Callback(_) => Err(Self::unsupported("callback parameter")),
                NativeParameterKind::Closure(_) => Err(Self::unsupported("closure parameter")),
                NativeParameterKind::Continuation(_) => {
                    Err(Self::unsupported("async continuation parameter"))
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let wrapper_returns = match method.returns() {
            NativeReturn::Void => "void".to_owned(),
            NativeReturn::Value(scalar) => JavaCType::from_jni(scalar.jni_type()).java().to_owned(),
            NativeReturn::Bytes => return Err(Self::unsupported("owned buffer return")),
            NativeReturn::Record(_) => return Err(Self::unsupported("direct record return")),
            NativeReturn::Callback(_) => return Err(Self::unsupported("callback handle return")),
            NativeReturn::Status => return Err(Self::unsupported("status return")),
            NativeReturn::StatusWriteback(_) => {
                return Err(Self::unsupported("status writeback return"));
            }
            NativeReturn::StatusValue(_) => return Err(Self::unsupported("status value return")),
            NativeReturn::EncodedError => return Err(Self::unsupported("encoded error return")),
            NativeReturn::EncodedErrorValue(_) => {
                return Err(Self::unsupported("encoded error value return"));
            }
        };
        Ok(Self {
            lib_name,
            c_function,
            needs_shim,
            transition,
            lib_params,
            lib_returns,
            wrapper_params,
            wrapper_returns,
            body: WrapperBody::Scalar,
        })
    }

    fn unsupported(shape: &'static str) -> Error {
        Error::UnsupportedTarget {
            target: NATIVE_IMAGE_BRIDGE,
            shape,
        }
    }

    /// Returns the `@CFunction` symbol name (the C export or its shim).
    pub fn lib_name(&self) -> &Identifier {
        &self.lib_name
    }

    /// Returns the C bridge function this downcall reaches.
    pub fn c_function(&self) -> &c::Function {
        &self.c_function
    }

    /// Returns whether the downcall targets a pointer-ABI shim.
    pub const fn needs_shim(&self) -> bool {
        self.needs_shim
    }

    /// Returns the thread transition used for this call.
    pub const fn transition(&self) -> Transition {
        self.transition
    }

    /// Returns whether the `@CFunction` skips the native transition.
    pub const fn no_transition(&self) -> bool {
        matches!(self.transition, Transition::NoTransition)
    }

    /// Returns `@CFunction` parameters.
    pub fn lib_params(&self) -> &[LibParameter] {
        &self.lib_params
    }

    /// Returns the `@CFunction` return type.
    pub const fn lib_returns(&self) -> JavaCType {
        self.lib_returns
    }

    /// Returns the Java wrapper method name (equal to the C export name).
    pub fn wrapper_name(&self) -> &str {
        self.c_function.name()
    }

    /// Returns Java wrapper parameters.
    pub fn wrapper_params(&self) -> &[WrapperParameter] {
        &self.wrapper_params
    }

    /// Returns the Java wrapper return type.
    pub fn wrapper_returns(&self) -> &str {
        &self.wrapper_returns
    }

    /// Returns whether the wrapper returns nothing.
    pub fn wrapper_returns_void(&self) -> bool {
        self.wrapper_returns == "void"
    }

    /// Returns how the wrapper body marshals values.
    pub fn body(&self) -> &WrapperBody {
        &self.body
    }

    /// Returns the comma-separated `@CFunction` parameter declarations.
    pub fn lib_declarations(&self) -> String {
        self.lib_params
            .iter()
            .map(LibParameter::declaration)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Returns the comma-separated wrapper parameter declarations.
    pub fn wrapper_declarations(&self) -> String {
        self.wrapper_params
            .iter()
            .map(WrapperParameter::declaration)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Returns the comma-separated wrapper argument names.
    pub fn wrapper_arguments(&self) -> String {
        self.wrapper_params
            .iter()
            .map(WrapperParameter::name)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

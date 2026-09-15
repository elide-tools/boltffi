//! One `@CFunction` declaration and the Java wrapper that calls it.
//!
//! A downcall pairs a C bridge function with its Native Image spelling. When
//! the C signature carries by-value structs, the downcall targets a
//! `boltffi_ni_*` shim with pointers instead, and the wrapper allocates the
//! storage. The wrapper keeps the exact Java signature the JNI `Native` class
//! exposes so host renderers do not change between bindings.
//!
//! Wrapper bodies are produced here as Java statements rather than in
//! templates, because the marshalling decisions (pin an array, read a direct
//! buffer address, allocate stack storage, translate a status) are made from
//! typed JNI-contract facts and are easier to test as data.

use crate::{
    bridge::{
        c::{self, Identifier},
        jni::{
            BytesParameter, CallbackHandleLifecycle, CallbackHandleMethod,
            ClosureParameter as JniClosureParameter, DirectVectorParameter, JniType, NativeMethod,
            NativeParameter, NativeParameterKind, NativeReturn, RecordParameter, SuccessOutReturn,
            SuccessOutValue, SuccessOutWriter,
        },
    },
    core::{Error, Result},
};

use super::{
    super::NATIVE_IMAGE_BRIDGE, ClosureUpcall, JavaCType, ShimFunction, shim::Pointerized,
};

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
    fn new(name: &str, java_type: impl Into<String>) -> Self {
        Self {
            name: java_identifier(name),
            java_type: java_type.into(),
        }
    }

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

/// Java wrapper with the JNI `Native` signature, implemented over `Lib`.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Wrapper {
    name: String,
    params: Vec<WrapperParameter>,
    returns: String,
    body: Vec<String>,
}

impl Wrapper {
    /// Returns the Java method name (equal to the C export name).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns Java parameters.
    pub fn params(&self) -> &[WrapperParameter] {
        &self.params
    }

    /// Returns the Java return type.
    pub fn returns(&self) -> &str {
        &self.returns
    }

    /// Returns Java statements forming the method body.
    pub fn body(&self) -> &[String] {
        &self.body
    }

    /// Java `Native.<clone>(long handle)` over a Rust-owned callback handle.
    pub(super) fn lifecycle_clone(lifecycle: &CallbackHandleLifecycle) -> Result<Self> {
        Ok(Self {
            name: lifecycle.clone_method().as_str().to_owned(),
            params: vec![WrapperParameter::new("handle", "long")],
            returns: "long".to_owned(),
            body: vec!["return cloneOwnedHandle(handle);".to_owned()],
        })
    }

    /// Java `Native.<release>(long handle)` over a Rust-owned callback handle.
    pub(super) fn lifecycle_release(lifecycle: &CallbackHandleLifecycle) -> Result<Self> {
        Ok(Self {
            name: lifecycle.release_method().as_str().to_owned(),
            params: vec![WrapperParameter::new("handle", "long")],
            returns: "void".to_owned(),
            body: vec!["releaseOwnedHandle(handle);".to_owned()],
        })
    }

    /// Java `Native.boltffi_success_<kind>(long returnOut, T value)` writing a fallible success value.
    pub(super) fn success_out_writer(writer: &SuccessOutWriter) -> Result<Self> {
        let (java_type, body) = match writer.value() {
            SuccessOutValue::Scalar { jni_type, .. } => {
                let java = JavaCType::from_jni(*jni_type);
                let pointer = scalar_pointer_type(java)?;
                let value = match jni_type {
                    JniType::Boolean => "(byte) (value ? 1 : 0)".to_owned(),
                    _ => "value".to_owned(),
                };
                (
                    java.java().to_owned(),
                    format!("(({pointer}) WordFactory.pointer(returnOut)).write({value});"),
                )
            }
            SuccessOutValue::Bytes => (
                "byte[]".to_owned(),
                "writeBuffer((FfiBuf) WordFactory.pointer(returnOut), value);".to_owned(),
            ),
            SuccessOutValue::Record { c_type } => (
                "byte[]".to_owned(),
                format!(
                    "writeRecord(WordFactory.pointer(returnOut), value, SizeOf.get({c_type}Struct.class));"
                ),
            ),
        };
        Ok(Self {
            name: writer.method().as_str().to_owned(),
            params: vec![
                WrapperParameter::new("returnOut", "long"),
                WrapperParameter::new("value", java_type),
            ],
            returns: "void".to_owned(),
            body: vec![
                "if (returnOut == 0L) throw new RuntimeException(\"BoltFFI success out pointer was null\");".to_owned(),
                body,
            ],
        })
    }

    /// Returns the comma-separated parameter declarations.
    pub fn declarations(&self) -> String {
        self.params
            .iter()
            .map(WrapperParameter::declaration)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A `@CFunction` declaration plus its optional Java wrapper.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Downcall {
    lib_name: Identifier,
    c_function: c::Function,
    needs_shim: bool,
    transition: Transition,
    lib_params: Vec<LibParameter>,
    lib_returns: JavaCType,
    wrapper: Option<Wrapper>,
    declares_lib: bool,
    extra_shim: Option<ShimFunction>,
}

/// Argument expressions passed to `Lib`, one per C ABI parameter, plus setup and teardown.
#[derive(Default)]
struct Marshalled {
    setup: Vec<String>,
    arguments: Vec<String>,
    /// Statements closing `try` blocks opened in `setup` (one `}` per pin).
    pins: usize,
}

impl Downcall {
    /// Builds the `@CFunction` declaration (and shim, if needed) for a C bridge function without a wrapper.
    pub fn from_c_function(function: &c::Function, transition: Transition) -> Result<Self> {
        let needs_shim = function
            .params()
            .iter()
            .any(|parameter| JavaCType::is_by_value_struct(parameter.ty()))
            || JavaCType::is_by_value_struct(function.returns());
        let lib_name = match needs_shim {
            true => Identifier::parse(format!("boltffi_ni_{}", function.name()))?,
            false => Identifier::parse(function.name())?,
        };
        let mut lib_params = function
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
        let lib_returns = match function.returns() {
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
        Ok(Self {
            lib_name,
            c_function: function.clone(),
            needs_shim,
            transition,
            lib_params,
            lib_returns,
            wrapper: None,
            declares_lib: true,
            extra_shim: None,
        })
    }

    /// Builds a downcall with a JNI-signature Java wrapper from one native method.
    pub fn from_native_method(
        method: &NativeMethod,
        transition: Transition,
        callbacks: &[c::Callback],
        closures: &[ClosureUpcall],
    ) -> Result<Self> {
        let mut downcall = Self::from_c_function(method.c_function(), transition)?;
        let wrapper = WrapperBuilder {
            downcall: &downcall,
            name: method.c_function().name().to_owned(),
            c_function: method.c_function(),
            parameters: method.parameters(),
            returns: method.returns(),
            success_out: method.success_out(),
            receiver: false,
            callbacks,
            closures,
        }
        .build()?;
        downcall.wrapper = Some(wrapper);
        Ok(downcall)
    }

    /// Builds the wrapper and glue shim for one Rust-owned callback handle method.
    ///
    /// The Java wrapper takes the handle token first and calls a glue shim that
    /// dispatches through the stored vtable slot with the handle's own value.
    pub fn from_handle_method(
        method: &CallbackHandleMethod,
        transition: Transition,
        callbacks: &[c::Callback],
        closures: &[ClosureUpcall],
    ) -> Result<Self> {
        if method.completion().is_some() {
            return Err(Self::unsupported("asynchronous callback handle method"));
        }
        if method.returns_closure() {
            return Err(Self::unsupported(
                "callback handle method returning a closure",
            ));
        }
        let returns = method
            .synchronous_return()
            .ok_or(Error::BrokenBridgeContract {
                bridge: NATIVE_IMAGE_BRIDGE,
                invariant: "synchronous callback handle method has a return contract",
            })?;
        let function = method.c_function();
        let lib_name = Identifier::parse(format!(
            "boltffi_ni_invoke_{}_{}",
            method.vtable_type().as_str(),
            method.slot().as_str()
        ))?;
        let pointerized = Pointerized::new(function.params(), function.returns())?;
        let lib_params = std::iter::once(Ok(LibParameter {
            name: Identifier::parse("__callback")?,
            ty: JavaCType::Pointer,
            c_type: c::Type::ConstPointer(Box::new(c::Type::Void)),
        }))
        .chain(pointerized.params.iter().map(|(name, ty, java)| {
            Ok(LibParameter {
                name: name.clone(),
                ty: *java,
                c_type: ty.clone(),
            })
        }))
        .collect::<Result<Vec<_>>>()?;
        let shim = ShimFunction::for_handle_method(
            lib_name.as_str(),
            method.vtable_type().as_str(),
            method.slot().as_str(),
            function,
            &pointerized,
        )?;
        let mut downcall = Self {
            lib_name,
            c_function: function.clone(),
            needs_shim: false,
            transition,
            lib_params,
            lib_returns: pointerized.returns,
            wrapper: None,
            declares_lib: true,
            extra_shim: Some(shim),
        };
        let wrapper = WrapperBuilder {
            downcall: &downcall,
            name: method.method().as_str().to_owned(),
            c_function: function,
            parameters: method.parameters(),
            returns,
            success_out: method.success_out(),
            receiver: true,
            callbacks,
            closures,
        }
        .build()?;
        downcall.wrapper = Some(wrapper);
        Ok(downcall)
    }

    /// A Java-only wrapper with no `@CFunction` of its own (lifecycle and writer helpers).
    pub(super) fn synthetic(wrapper: Wrapper) -> Self {
        Self {
            lib_name: Identifier::parse("boltffi_ni_synthetic").expect("static identifier"),
            c_function: c::Function::new("boltffi_ni_synthetic", Vec::new(), c::Type::Void)
                .expect("static function"),
            needs_shim: false,
            transition: Transition::NoTransition,
            lib_params: Vec::new(),
            lib_returns: JavaCType::Void,
            wrapper: Some(wrapper),
            declares_lib: false,
            extra_shim: None,
        }
    }

    /// Returns a hand-written glue shim replacing the automatic pointer-ABI one.
    pub fn extra_shim(&self) -> Option<&ShimFunction> {
        self.extra_shim.as_ref()
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

    /// Returns whether a `@CFunction` is declared for this downcall.
    pub const fn declares_lib(&self) -> bool {
        self.declares_lib
    }

    /// Returns the Java wrapper, when this downcall backs a JNI-contract native method.
    pub fn wrapper(&self) -> Option<&Wrapper> {
        self.wrapper.as_ref()
    }

    /// Returns the comma-separated `@CFunction` parameter declarations.
    pub fn lib_declarations(&self) -> String {
        self.lib_params
            .iter()
            .map(LibParameter::declaration)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Builds the wrapper body for one native method or callback handle method.
struct WrapperBuilder<'a> {
    downcall: &'a Downcall,
    /// Java method name (the JNI `Native` member this wrapper replaces).
    name: String,
    /// C shape of the parameters the Java wrapper marshals.
    c_function: &'a c::Function,
    parameters: &'a [NativeParameter],
    returns: &'a NativeReturn,
    success_out: Option<&'a SuccessOutReturn>,
    /// Whether a leading `long handle` names a Rust-owned callback to dispatch through.
    receiver: bool,
    callbacks: &'a [c::Callback],
    closures: &'a [ClosureUpcall],
}

impl WrapperBuilder<'_> {
    fn build(self) -> Result<Wrapper> {
        let mut params = Vec::new();
        let mut marshalled = Marshalled::default();
        if self.receiver {
            params.push(WrapperParameter::new("handle", "long"));
            marshalled
                .setup
                .push("CallbackHandle __callback = ownedHandle(handle);".to_owned());
            marshalled.arguments.push("__callback".to_owned());
        }
        let groups = self.c_function.parameter_groups().iter();
        let mut parameters = self.parameters.iter();
        let mut success_out: Option<&SuccessOutReturn> = None;
        let mut writeback_buffer: Option<String> = None;
        for group in groups {
            match group {
                c::ParameterGroup::SuccessOut(_) => {
                    let out = self.success_out.ok_or(Error::BrokenBridgeContract {
                        bridge: NATIVE_IMAGE_BRIDGE,
                        invariant: "success out-pointer has a matching JNI return value",
                    })?;
                    success_out = Some(out);
                    Self::success_out_storage(out, &mut marshalled)?;
                }
                c::ParameterGroup::CompletionStatusOut(_) => {
                    return Err(Downcall::unsupported("async completion status"));
                }
                c::ParameterGroup::CallbackCompletion(_) | c::ParameterGroup::ClosureReturn(_) => {
                    return Err(Error::BrokenBridgeContract {
                        bridge: NATIVE_IMAGE_BRIDGE,
                        invariant: "non-native parameter group cannot appear on a native method",
                    });
                }
                _ => {
                    let parameter = parameters.next().ok_or(Error::BrokenBridgeContract {
                        bridge: NATIVE_IMAGE_BRIDGE,
                        invariant: "native method has a parameter for every C group",
                    })?;
                    match parameter.kind() {
                        NativeParameterKind::Scalar(scalar) => {
                            let param = WrapperParameter::new(
                                parameter.name().as_str(),
                                JavaCType::from_jni(scalar.ty()).java(),
                            );
                            marshalled.arguments.push(param.name.clone());
                            params.push(param);
                        }
                        NativeParameterKind::Bytes(bytes) => {
                            Self::bytes(
                                bytes,
                                &mut params,
                                &mut marshalled,
                                &mut writeback_buffer,
                            )?;
                        }
                        NativeParameterKind::DirectVector(vector) => {
                            Self::direct_vector(vector, &mut params, &mut marshalled);
                        }
                        NativeParameterKind::Record(record) => {
                            Self::record(record, group, &mut params, &mut marshalled);
                        }
                        NativeParameterKind::Callback(callback) => {
                            let c::ParameterGroup::Value(index) = group else {
                                return Err(Error::BrokenBridgeContract {
                                    bridge: NATIVE_IMAGE_BRIDGE,
                                    invariant: "callback parameter is a single C value",
                                });
                            };
                            let c::Type::CallbackHandle(id) =
                                self.c_function.parameter(*index).ty()
                            else {
                                return Err(Error::BrokenBridgeContract {
                                    bridge: NATIVE_IMAGE_BRIDGE,
                                    invariant: "callback parameter has a callback handle type",
                                });
                            };
                            let stem = self
                                .callbacks
                                .iter()
                                .find(|declaration| declaration.id() == *id)
                                .map(|declaration| declaration.vtable().name().to_owned())
                                .ok_or(Error::BrokenBridgeContract {
                                    bridge: NATIVE_IMAGE_BRIDGE,
                                    invariant: "callback parameter names a declared callback",
                                })?;
                            let token = WrapperParameter::new(callback.name().as_str(), "long");
                            let local = format!("__cb_{}", token.name);
                            marshalled.setup.push(format!(
                                "CallbackHandle {local} = StackValue.get(CallbackHandle.class);"
                            ));
                            marshalled
                                .setup
                                .push(format!("{stem}_parameter({}, {local});", token.name));
                            marshalled.arguments.push(local);
                            params.push(token);
                        }
                        NativeParameterKind::Closure(closure) => {
                            self.closure(closure, &mut params, &mut marshalled)?;
                        }
                        NativeParameterKind::Continuation(_) => {
                            return Err(Downcall::unsupported("async continuation parameter"));
                        }
                    }
                }
            }
        }
        let (returns, mut body, tail) =
            self.return_shape(&mut marshalled, success_out, writeback_buffer.as_deref())?;
        let mut statements = marshalled.setup;
        statements.append(&mut body);
        for _ in 0..marshalled.pins {
            statements.push("}".to_owned());
        }
        statements.extend(tail);
        Ok(Wrapper {
            name: self.name.clone(),
            params,
            returns,
            body: statements,
        })
    }

    fn call(&self, marshalled: &Marshalled) -> String {
        format!(
            "Lib.{}({})",
            self.downcall.lib_name().as_str(),
            marshalled.arguments.join(", ")
        )
    }

    /// Passes a Java closure token to C as a `(call, cell, release)` triple.
    fn closure(
        &self,
        closure: &JniClosureParameter,
        params: &mut Vec<WrapperParameter>,
        marshalled: &mut Marshalled,
    ) -> Result<()> {
        let arguments = closure.c_arguments();
        let call =
            arguments
                .first()
                .map(ToString::to_string)
                .ok_or(Error::BrokenBridgeContract {
                    bridge: NATIVE_IMAGE_BRIDGE,
                    invariant: "closure parameter passes a call function pointer",
                })?;
        let upcall = self
            .closures
            .iter()
            .find(|upcall| upcall.jni_call() == call)
            .ok_or(Error::BrokenBridgeContract {
                bridge: NATIVE_IMAGE_BRIDGE,
                invariant: "closure parameter names a registered closure signature",
            })?;
        let token = WrapperParameter::new(closure.name().as_str(), "long");
        marshalled
            .arguments
            .push(format!("Lib.{}()", upcall.call_address()));
        marshalled.arguments.push(format!(
            "WordFactory.pointer(newHandleCell({}))",
            token.name
        ));
        marshalled
            .arguments
            .push(format!("Lib.{}()", upcall.release_address()));
        params.push(token);
        Ok(())
    }

    fn bytes(
        bytes: &BytesParameter,
        params: &mut Vec<WrapperParameter>,
        marshalled: &mut Marshalled,
        writeback_buffer: &mut Option<String>,
    ) -> Result<()> {
        let buffer = WrapperParameter::new(bytes.name().as_str(), "java.nio.ByteBuffer");
        let length = WrapperParameter::new(bytes.length().as_str(), "int");
        marshalled.setup.push(format!(
            "PointerBase {ptr} = WordFactory.pointer(directAddress({buffer}, {length}));",
            ptr = bytes.pointer().as_str(),
            buffer = buffer.name,
            length = length.name,
        ));
        marshalled
            .arguments
            .push(bytes.pointer().as_str().to_owned());
        marshalled.arguments.push(format!("(long) {}", length.name));
        if let Some(writeback) = bytes.writeback() {
            let local = writeback.local().as_str().to_owned();
            marshalled
                .setup
                .push(format!("FfiBuf {local} = StackValue.get(FfiBuf.class);"));
            marshalled.setup.push(format!("zeroBuffer({local});"));
            marshalled.arguments.push(local.clone());
            *writeback_buffer = Some(local);
        }
        params.push(buffer);
        params.push(length);
        Ok(())
    }

    fn direct_vector(
        vector: &DirectVectorParameter,
        params: &mut Vec<WrapperParameter>,
        marshalled: &mut Marshalled,
    ) {
        let element = JavaCType::from_jni(vector.jni_type()).java();
        let array = WrapperParameter::new(vector.name().as_str(), format!("{element}[]"));
        let pin = format!("__pin_{}", array.name);
        marshalled.setup.push(format!(
            "if ({} == null) throw new IllegalArgumentException(\"BoltFFI array argument was null\");",
            array.name
        ));
        marshalled.setup.push(format!(
            "try (PinnedObject {pin} = PinnedObject.create({})) {{",
            array.name
        ));
        marshalled.pins += 1;
        marshalled
            .arguments
            .push(format!("{pin}.addressOfArrayElement(0)"));
        marshalled
            .arguments
            .push(format!("(long) {}.length", array.name));
        params.push(array);
    }

    fn record(
        record: &RecordParameter,
        group: &c::ParameterGroup,
        params: &mut Vec<WrapperParameter>,
        marshalled: &mut Marshalled,
    ) {
        let buffer = WrapperParameter::new(record.name().as_str(), "java.nio.ByteBuffer");
        let pointer = record.pointer().as_str().to_owned();
        marshalled.setup.push(format!(
            "PointerBase {pointer} = WordFactory.pointer(directAddress({}, SizeOf.get({}Struct.class)));",
            buffer.name,
            record.c_type().as_str(),
        ));
        marshalled.arguments.push(pointer.clone());
        if matches!(group, c::ParameterGroup::DirectWriteback(_)) {
            // The C function copies the input by value before writing the output,
            // so the same buffer serves as both the input and the writeback target.
            marshalled.arguments.push(pointer);
        }
        params.push(buffer);
    }

    fn success_out_storage(out: &SuccessOutReturn, marshalled: &mut Marshalled) -> Result<()> {
        let local = out.local().as_str().to_owned();
        match out.value() {
            NativeReturn::Value(scalar) => {
                let pointer = scalar_pointer_type(JavaCType::from_jni(scalar.jni_type()))?;
                marshalled.setup.push(format!(
                    "{pointer} {local} = StackValue.get({pointer}.class);"
                ));
                marshalled.arguments.push(local);
            }
            NativeReturn::Bytes => {
                marshalled
                    .setup
                    .push(format!("FfiBuf {local} = StackValue.get(FfiBuf.class);"));
                marshalled.setup.push(format!("zeroBuffer({local});"));
                marshalled.arguments.push(local);
            }
            NativeReturn::Record(record) => {
                let pin = format!("__pin_{local}");
                marshalled.setup.push(format!(
                    "byte[] {local} = new byte[SizeOf.get({}Struct.class)];",
                    record.c_type().as_str()
                ));
                marshalled.setup.push(format!(
                    "try (PinnedObject {pin} = PinnedObject.create({local})) {{"
                ));
                marshalled.pins += 1;
                marshalled
                    .arguments
                    .push(format!("{pin}.addressOfArrayElement(0)"));
            }
            NativeReturn::Callback(_) => {
                return Err(Downcall::unsupported("callback handle success value"));
            }
            _ => {
                return Err(Error::BrokenBridgeContract {
                    bridge: NATIVE_IMAGE_BRIDGE,
                    invariant: "success out value is a scalar, buffer, or record",
                });
            }
        }
        Ok(())
    }

    /// Returns `(java return type, statements inside pins, statements after pins)`.
    fn return_shape(
        &self,
        marshalled: &mut Marshalled,
        success_out: Option<&SuccessOutReturn>,
        writeback_buffer: Option<&str>,
    ) -> Result<(String, Vec<String>, Vec<String>)> {
        let returns = self.returns;
        let mut body = Vec::new();
        let mut tail = Vec::new();
        let result_type = match returns {
            NativeReturn::Void => "void".to_owned(),
            NativeReturn::Value(scalar) => JavaCType::from_jni(scalar.jni_type()).java().to_owned(),
            NativeReturn::Bytes | NativeReturn::Record(_) | NativeReturn::StatusWriteback(_) => {
                "byte[]".to_owned()
            }
            NativeReturn::Status | NativeReturn::EncodedError => "void".to_owned(),
            NativeReturn::StatusValue(out) => Self::success_java_type(out)?,
            NativeReturn::EncodedErrorValue(out) => Self::success_java_type(out.success())?,
            NativeReturn::Callback(_) => "long".to_owned(),
        };
        let needs_result_local = marshalled.pins > 0 && result_type != "void";
        let finish = |body: &mut Vec<String>, tail: &mut Vec<String>, value: String| {
            if needs_result_local {
                body.push(format!("__result = {value};"));
                tail.push("return __result;".to_owned());
            } else {
                body.push(format!("return {value};"));
            }
        };
        match returns {
            NativeReturn::Void => body.push(format!("{};", self.call(marshalled))),
            NativeReturn::Value(_) => finish(&mut body, &mut tail, self.call(marshalled)),
            NativeReturn::Bytes => {
                marshalled.arguments.push("__result_buffer".to_owned());
                body.push("FfiBuf __result_buffer = StackValue.get(FfiBuf.class);".to_owned());
                body.push("zeroBuffer(__result_buffer);".to_owned());
                body.push(format!("{};", self.call(marshalled)));
                finish(
                    &mut body,
                    &mut tail,
                    "takeBuffer(__result_buffer)".to_owned(),
                );
            }
            NativeReturn::Record(record) => {
                marshalled
                    .arguments
                    .push("__pin_result.addressOfArrayElement(0)".to_owned());
                body.push(format!(
                    "byte[] __result_record = new byte[SizeOf.get({}Struct.class)];",
                    record.c_type().as_str()
                ));
                body.push(
                    "try (PinnedObject __pin_result = PinnedObject.create(__result_record)) {"
                        .to_owned(),
                );
                body.push(format!("    {};", self.call(marshalled)));
                body.push("}".to_owned());
                finish(&mut body, &mut tail, "__result_record".to_owned());
            }
            NativeReturn::Status => {
                body.push(format!("checkStatus({});", self.call(marshalled)));
            }
            NativeReturn::StatusWriteback(_) => {
                let buffer = writeback_buffer.ok_or(Error::BrokenBridgeContract {
                    bridge: NATIVE_IMAGE_BRIDGE,
                    invariant: "status writeback has an encoded writeback parameter",
                })?;
                body.push(format!("checkStatus({});", self.call(marshalled)));
                finish(&mut body, &mut tail, format!("takeBuffer({buffer})"));
            }
            NativeReturn::StatusValue(out) => {
                body.push(format!("checkStatus({});", self.call(marshalled)));
                finish(&mut body, &mut tail, Self::success_value(out)?);
            }
            NativeReturn::EncodedError => {
                marshalled.arguments.push("__error".to_owned());
                body.push("FfiBuf __error = StackValue.get(FfiBuf.class);".to_owned());
                body.push("zeroBuffer(__error);".to_owned());
                body.push(format!("{};", self.call(marshalled)));
                body.push("checkErrorBuffer(__error);".to_owned());
            }
            NativeReturn::EncodedErrorValue(out) => {
                marshalled.arguments.push("__error".to_owned());
                body.push("FfiBuf __error = StackValue.get(FfiBuf.class);".to_owned());
                body.push("zeroBuffer(__error);".to_owned());
                body.push(format!("{};", self.call(marshalled)));
                body.push("checkErrorBuffer(__error);".to_owned());
                finish(&mut body, &mut tail, Self::success_value(out.success())?);
            }
            NativeReturn::Callback(_) => {
                marshalled.arguments.push("__result_handle".to_owned());
                body.push(
                    "CallbackHandle __result_handle = StackValue.get(CallbackHandle.class);"
                        .to_owned(),
                );
                body.push(format!("{};", self.call(marshalled)));
                finish(
                    &mut body,
                    &mut tail,
                    "storeOwnedHandle(__result_handle)".to_owned(),
                );
            }
        }
        let _ = success_out;
        if needs_result_local {
            marshalled
                .setup
                .insert(0, format!("{result_type} __result;"));
        }
        Ok((result_type, body, tail))
    }

    fn success_java_type(out: &SuccessOutReturn) -> Result<String> {
        Ok(match out.value() {
            NativeReturn::Value(scalar) => JavaCType::from_jni(scalar.jni_type()).java().to_owned(),
            NativeReturn::Bytes | NativeReturn::Record(_) => "byte[]".to_owned(),
            NativeReturn::Callback(_) => {
                return Err(Downcall::unsupported("callback handle success value"));
            }
            _ => {
                return Err(Error::BrokenBridgeContract {
                    bridge: NATIVE_IMAGE_BRIDGE,
                    invariant: "success out value is a scalar, buffer, or record",
                });
            }
        })
    }

    fn success_value(out: &SuccessOutReturn) -> Result<String> {
        let local = out.local().as_str();
        Ok(match out.value() {
            NativeReturn::Value(scalar) => match JavaCType::from_jni(scalar.jni_type()) {
                JavaCType::Boolean => format!("{local}.read() != 0"),
                _ => format!("{local}.read()"),
            },
            NativeReturn::Bytes => format!("takeBuffer({local})"),
            NativeReturn::Record(_) => local.to_owned(),
            _ => {
                return Err(Error::BrokenBridgeContract {
                    bridge: NATIVE_IMAGE_BRIDGE,
                    invariant: "success out value is a scalar, buffer, or record",
                });
            }
        })
    }
}

/// Returns the `org.graalvm.nativeimage.c.type` pointer class for a scalar.
fn scalar_pointer_type(ty: JavaCType) -> Result<&'static str> {
    Ok(match ty {
        JavaCType::Boolean | JavaCType::Byte => "org.graalvm.nativeimage.c.type.CCharPointer",
        JavaCType::Short => "org.graalvm.nativeimage.c.type.CShortPointer",
        JavaCType::Int => "org.graalvm.nativeimage.c.type.CIntPointer",
        JavaCType::Long => "org.graalvm.nativeimage.c.type.CLongPointer",
        JavaCType::Float => "org.graalvm.nativeimage.c.type.CFloatPointer",
        JavaCType::Double => "org.graalvm.nativeimage.c.type.CDoublePointer",
        JavaCType::Void | JavaCType::Pointer => {
            return Err(Error::BrokenBridgeContract {
                bridge: NATIVE_IMAGE_BRIDGE,
                invariant: "scalar success value has a pointer type",
            });
        }
    })
}

const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "true",
    "false",
    "null",
    "var",
    "record",
    "yield",
    "_",
];

/// Makes a C identifier safe as a Java identifier.
fn java_identifier(name: &str) -> String {
    match JAVA_KEYWORDS.contains(&name) {
        true => format!("{name}_"),
        false => name.to_owned(),
    }
}

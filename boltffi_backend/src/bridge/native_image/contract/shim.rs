//! Pointer-ABI C shims for by-value struct shapes.
//!
//! `@CFunction` and `@CEntryPoint` cannot pass or return structs by value, but
//! the C ABI bridge does so for owned buffers, direct records, callback
//! handles, and status codes. This module rewrites a C signature into its
//! pointer form: by-value parameters become `const T *`, a by-value return
//! becomes a trailing `T *__out`, and `FfiStatus` becomes its `int32_t` code.
//! Downcall shims wrap C exports for Java to call; upcall shims wrap Java entry
//! points so Rust can call them through a vtable.

use crate::{
    bridge::c::{self, Identifier, TypeFragment},
    core::Result,
};

use super::{Downcall, JavaCType};

/// A C signature rewritten so every struct travels behind a pointer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Pointerized {
    /// Pointer-form parameters: `(name, pointerized C type, Java type)`.
    pub(super) params: Vec<(Identifier, c::Type, JavaCType)>,
    /// Pointer-form return type (`Void` when the original returned a struct).
    pub(super) returns: JavaCType,
    /// Whether a trailing `__out` pointer carries the original by-value return.
    pub(super) out_param: bool,
    /// Whether the original returned `FfiStatus`, now spelled `int32_t`.
    pub(super) status_return: bool,
    /// Original return type.
    pub(super) original_returns: c::Type,
}

impl Pointerized {
    /// Rewrites `params -> returns` into pointer form.
    pub(super) fn new(params: &[c::Parameter], returns: &c::Type) -> Result<Self> {
        let mut pointerized = params
            .iter()
            .map(|parameter| {
                let (ty, java) = match JavaCType::is_by_value_struct(parameter.ty()) {
                    true => (
                        c::Type::ConstPointer(Box::new(parameter.ty().clone())),
                        JavaCType::Pointer,
                    ),
                    false => (parameter.ty().clone(), JavaCType::from_c(parameter.ty())?),
                };
                Ok((Identifier::parse(parameter.name())?, ty, java))
            })
            .collect::<Result<Vec<_>>>()?;
        let status_return = matches!(returns, c::Type::Status);
        let out_param = !status_return && JavaCType::is_by_value_struct(returns);
        if out_param {
            pointerized.push((
                Identifier::parse("__out")?,
                c::Type::MutPointer(Box::new(returns.clone())),
                JavaCType::Pointer,
            ));
        }
        Ok(Self {
            params: pointerized,
            returns: match (status_return, out_param) {
                (true, _) => JavaCType::Int,
                (_, true) => JavaCType::Void,
                _ => JavaCType::from_c(returns)?,
            },
            out_param,
            status_return,
            original_returns: returns.clone(),
        })
    }

    /// C parameter list in pointer form, e.g. `const Point *point, double by`.
    pub(super) fn c_declarations(&self) -> Result<String> {
        Ok(self
            .params
            .iter()
            .map(|(name, ty, _)| c_declaration(ty, name.as_str()))
            .collect::<Result<Vec<_>>>()?
            .join(", "))
    }

    /// C return type in pointer form (`int32_t`, `void`, or the scalar).
    pub(super) fn c_return(&self) -> Result<String> {
        Ok(match (self.status_return, self.out_param) {
            (true, _) => "int32_t".to_owned(),
            (_, true) => "void".to_owned(),
            _ => TypeFragment::anonymous(&self.original_returns)?.to_string(),
        })
    }

    /// Java `@CFunction`/`@CEntryPoint` parameter declarations.
    pub(super) fn java_declarations(&self) -> String {
        self.params
            .iter()
            .map(|(name, _, java)| format!("{} {}", java.java(), name.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Spells a C parameter declaration, wrapping function pointers around the name.
pub(super) fn c_declaration(ty: &c::Type, name: &str) -> Result<String> {
    Ok(TypeFragment::declaration(ty, name)?.to_string())
}

/// One shim function definition in the generated glue file.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ShimFunction {
    name: Identifier,
    source: String,
}

impl ShimFunction {
    /// Builds the downcall shim, or `None` when the export is called directly.
    pub fn for_downcall(downcall: &Downcall) -> Result<Option<Self>> {
        if !downcall.needs_shim() {
            return Ok(None);
        }
        let function = downcall.c_function();
        let pointerized = Pointerized::new(function.params(), function.returns())?;
        let arguments = function
            .params()
            .iter()
            .map(
                |parameter| match JavaCType::is_by_value_struct(parameter.ty()) {
                    true => format!("*{}", parameter.name()),
                    false => parameter.name().to_owned(),
                },
            )
            .collect::<Vec<_>>()
            .join(", ");
        let call = format!("{}({})", function.name(), arguments);
        let body = if pointerized.status_return {
            format!("return {call}.code;")
        } else if pointerized.out_param {
            format!("*__out = {call};")
        } else if pointerized.returns.is_void() {
            format!("{call};")
        } else {
            format!("return {call};")
        };
        let source = format!(
            "{} {}({}) {{\n    {body}\n}}",
            pointerized.c_return()?,
            downcall.lib_name().as_str(),
            pointerized.c_declarations()?,
        );
        Ok(Some(Self {
            name: downcall.lib_name().clone(),
            source,
        }))
    }

    /// Builds the glue shim that dispatches one Rust-owned callback handle slot.
    ///
    /// Java cannot follow a C function pointer out of a struct it only sees as a
    /// word, so the glue casts the stored vtable and calls the slot with the
    /// handle's own value, taking every by-value struct through a pointer.
    pub(super) fn for_handle_method(
        name: &str,
        vtable_type: &str,
        slot: &str,
        function: &c::Function,
        pointerized: &Pointerized,
    ) -> Result<Self> {
        let arguments = std::iter::once("__callback->handle".to_owned())
            .chain(function.params().iter().map(|parameter| {
                match JavaCType::is_by_value_struct(parameter.ty()) {
                    true => format!("*{}", parameter.name()),
                    false => parameter.name().to_owned(),
                }
            }))
            .collect::<Vec<_>>()
            .join(", ");
        let call = format!("__vtable->{slot}({arguments})");
        let body = if pointerized.status_return {
            format!("return {call}.code;")
        } else if pointerized.out_param {
            format!("*__out = {call};")
        } else if pointerized.returns.is_void() {
            format!("{call};")
        } else {
            format!("return {call};")
        };
        let declarations = std::iter::once("const BoltFFICallbackHandle *__callback".to_owned())
            .chain(
                std::iter::once(pointerized.c_declarations()?).filter(|params| !params.is_empty()),
            )
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!(
            "{returns} {name}({declarations}) {{\n    const {vtable_type} *__vtable = (const {vtable_type} *)__callback->vtable;\n    {body}\n}}",
            returns = pointerized.c_return()?,
        );
        Ok(Self {
            name: Identifier::parse(name)?,
            source,
        })
    }

    /// Creates a shim from an already-rendered definition.
    pub(super) fn raw(name: Identifier, source: String) -> Self {
        Self { name, source }
    }

    /// Returns the shim symbol.
    pub fn name(&self) -> &Identifier {
        &self.name
    }

    /// Returns the C definition.
    pub fn source(&self) -> &str {
        &self.source
    }
}

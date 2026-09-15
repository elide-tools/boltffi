//! Pointer-ABI C shims for by-value struct shapes.
//!
//! `@CFunction` cannot pass or return structs by value, but the C ABI bridge
//! does so for owned buffers, direct records, callback handles, and status
//! codes. Each affected export gets a `boltffi_ni_*` wrapper that takes
//! `const T *` for by-value parameters, writes a by-value return through a
//! trailing `T *__out`, and returns `FfiStatus` as its `int32_t` code.

use crate::{
    bridge::c::{self, Identifier, TypeFragment},
    core::Result,
};

use super::{Downcall, JavaCType};

/// One shim function definition in the generated glue file.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ShimFunction {
    name: Identifier,
    source: String,
}

impl ShimFunction {
    /// Builds the shim for a downcall, or `None` when the export is called directly.
    pub fn for_downcall(downcall: &Downcall) -> Result<Option<Self>> {
        if !downcall.needs_shim() {
            return Ok(None);
        }
        let function = downcall.c_function();
        let returns_status = matches!(function.returns(), c::Type::Status);
        let returns_struct = !returns_status && JavaCType::is_by_value_struct(function.returns());
        let parameters = downcall
            .lib_params()
            .iter()
            .map(|parameter| {
                Ok(format!(
                    "{} {}",
                    TypeFragment::anonymous(parameter.c_type())?,
                    parameter.name().as_str()
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join(", ");
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
        let (return_type, body) = if returns_status {
            ("int32_t".to_owned(), format!("return {call}.code;"))
        } else if returns_struct {
            ("void".to_owned(), format!("*__out = {call};"))
        } else {
            let returns = TypeFragment::anonymous(function.returns())?.to_string();
            match returns.as_str() {
                "void" => (returns, format!("{call};")),
                _ => (returns, format!("return {call};")),
            }
        };
        let source = format!(
            "{return_type} {name}({parameters}) {{\n    {body}\n}}",
            name = downcall.lib_name().as_str(),
        );
        Ok(Some(Self {
            name: downcall.lib_name().clone(),
            source,
        }))
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

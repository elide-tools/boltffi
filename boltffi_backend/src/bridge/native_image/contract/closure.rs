//! Closures: Rust calling a JVM-owned lambda through a `(call, context, release)` triple.
//!
//! A Java closure reaches C as three words. In Native Image mode the context is
//! the same unmanaged `HandleCell` used by callbacks, so the entry point
//! prologue can find the isolate and attach a foreign Rust thread. The call and
//! release slots are static C forwarders in the glue, exactly like callback
//! vtable slots, so struct-by-value arguments travel behind pointers.
//!
//! Java cannot take the address of a C function, so the glue also exports two
//! `void *` accessors per signature and the downcall wrapper reads them through
//! `@CFunction` when it builds the triple.

use crate::{
    bridge::{
        c::{self, Identifier},
        jni::ClosureRegistration,
    },
    core::{Error, Result},
};

use super::{
    super::NATIVE_IMAGE_BRIDGE,
    JavaCType, ShimFunction, Trampoline,
    shim::{Pointerized, c_declaration},
    upcall::{copy_helper, trampoline_body, unsupported},
};

/// The C ABI shape of one closure signature, recovered from a call function pointer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ClosureShape {
    pub(super) params: Vec<c::Parameter>,
    pub(super) returns: c::Type,
}

impl ClosureShape {
    /// Reads the shape out of a `void (*)(void *, ...)` closure call parameter type.
    pub(super) fn from_call_type(ty: &c::Type, params: &[c::Parameter]) -> Result<Self> {
        let c::Type::FunctionPointer { returns, .. } = ty else {
            return Err(Error::BrokenBridgeContract {
                bridge: NATIVE_IMAGE_BRIDGE,
                invariant: "closure call parameter has a function pointer type",
            });
        };
        Ok(Self {
            params: params.to_vec(),
            returns: returns.as_ref().clone(),
        })
    }
}

/// Everything one JVM-owned closure signature needs in Native Image mode.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ClosureUpcall {
    /// JNI C call symbol, used to match a native method's closure parameter.
    jni_call: String,
    /// Glue accessor returning the static call forwarder's address.
    call_address: String,
    /// Glue accessor returning the static release forwarder's address.
    release_address: String,
    trampolines: Vec<Trampoline>,
    shims: Vec<ShimFunction>,
}

impl ClosureUpcall {
    /// Builds the call and release trampolines plus glue forwarders for one signature.
    pub(super) fn new(registration: &ClosureRegistration, shape: &ClosureShape) -> Result<Self> {
        let stem = registration.signature().symbol_part();
        let dispatch_class = registration.class().class_name().to_owned();
        let pointerized = Pointerized::new(&shape.params, &shape.returns)?;
        let record_type = |name: &str| -> Option<String> {
            shape
                .params
                .iter()
                .find(|parameter| parameter.name() == name)
                .and_then(|parameter| match parameter.ty() {
                    c::Type::DirectRecord(id) => Some(id.as_str().to_owned()),
                    _ => None,
                })
        };

        let mut setup = Vec::new();
        let mut arguments = vec!["__cell.token()".to_owned()];
        for argument in registration.arguments() {
            if let Some(scalar) = argument.call_scalar() {
                arguments.push(scalar.parameter().name().as_str().to_owned());
            } else if let Some(bytes) = argument.call_bytes() {
                setup.push(format!(
                    "byte[] {} = copyBytes({}, {});",
                    bytes.name().as_str(),
                    bytes.pointer().as_str(),
                    bytes.length().as_str()
                ));
                arguments.push(bytes.name().as_str().to_owned());
            } else if let Some(vector) = argument.call_direct_vector() {
                setup.push(format!(
                    "{}[] {} = {}({}, {});",
                    JavaCType::from_jni(vector.jni_type()).java(),
                    vector.name().as_str(),
                    copy_helper(vector.jni_type()),
                    vector.pointer().as_str(),
                    vector.length().as_str()
                ));
                arguments.push(vector.name().as_str().to_owned());
            } else if let Some(record) = argument.call_record() {
                let name = record.parameter().name().as_str().to_owned();
                let c_type = record_type(&name).ok_or(Error::BrokenBridgeContract {
                    bridge: NATIVE_IMAGE_BRIDGE,
                    invariant: "closure record argument names a direct record parameter",
                })?;
                setup.push(format!(
                    "byte[] {} = copyRecord({name}, SizeOf.get({c_type}Struct.class));",
                    record.array().as_str(),
                ));
                arguments.push(record.array().as_str().to_owned());
            } else if let Some(out) = argument.success_out() {
                arguments.push(format!("{}.rawValue()", out.name().as_str()));
            } else if argument.call_closure().is_some() {
                return Err(unsupported("closure argument inside a closure"));
            } else {
                return Err(Error::BrokenBridgeContract {
                    bridge: NATIVE_IMAGE_BRIDGE,
                    invariant: "closure argument has a known kind",
                });
            }
        }
        if registration.returns_callback_handle() {
            return Err(unsupported("closure returning a callback handle"));
        }
        let call = format!("{dispatch_class}.call({})", arguments.join(", "));
        let body = trampoline_body(&call, &setup, &pointerized, &shape.returns, None)?;

        let call_entry = format!("boltffi_ni_java_{stem}_call");
        let release_entry = format!("boltffi_ni_java_{stem}_release");
        let call_forwarder = format!("boltffi_ni_{stem}_call_shim");
        let release_forwarder = format!("boltffi_ni_{stem}_release_shim");
        let call_address = format!("boltffi_ni_closure_{stem}_call");
        let release_address = format!("boltffi_ni_closure_{stem}_release");

        let trampolines = vec![
            Trampoline::new(
                call_entry.clone(),
                format!("{stem}_call"),
                pointerized.java_declarations(),
                pointerized.returns,
                body,
            ),
            Trampoline::new(
                release_entry.clone(),
                format!("{stem}_release"),
                String::new(),
                JavaCType::Void,
                vec![
                    "try {".to_owned(),
                    format!("    {dispatch_class}.free(__cell.token());"),
                    "} catch (Throwable __failure) {".to_owned(),
                    "    // A failed release has nowhere to report; the cell still goes away."
                        .to_owned(),
                    "}".to_owned(),
                    "UnmanagedMemory.free(__cell);".to_owned(),
                ],
            ),
        ];

        let c_params = std::iter::once(Ok("void *__context".to_owned()))
            .chain(
                shape
                    .params
                    .iter()
                    .map(|parameter| c_declaration(parameter.ty(), parameter.name())),
            )
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        let forwarded = std::iter::once("(BoltFFINativeImageHandleCell *)__context".to_owned())
            .chain(shape.params.iter().map(|parameter| {
                match JavaCType::is_by_value_struct(parameter.ty()) {
                    true => format!("&{}", parameter.name()),
                    false => parameter.name().to_owned(),
                }
            }))
            .chain(pointerized.out_param.then(|| "&__out".to_owned()))
            .collect::<Vec<_>>()
            .join(", ");
        let entry_params = std::iter::once("BoltFFINativeImageHandleCell *__context".to_owned())
            .chain(std::iter::once(pointerized.c_declarations()?).filter(|list| !list.is_empty()))
            .collect::<Vec<_>>()
            .join(", ");
        let c_return = c::TypeFragment::anonymous(&shape.returns)?.to_string();
        let entry_call = format!("{call_entry}({forwarded})");
        let forwarder_body = if pointerized.status_return {
            format!("return (FfiStatus){{ .code = {entry_call} }};")
        } else if pointerized.out_param {
            format!(
                "{c_return} __out;\n    memset(&__out, 0, sizeof(__out));\n    {entry_call};\n    return __out;"
            )
        } else if pointerized.returns.is_void() {
            format!("{entry_call};")
        } else {
            format!("return {entry_call};")
        };
        let shims = vec![
            ShimFunction::raw(
                Identifier::parse(&call_forwarder)?,
                format!(
                    "extern {} {call_entry}({entry_params});\nstatic {c_return} {call_forwarder}({c_params}) {{\n    {forwarder_body}\n}}",
                    pointerized.c_return()?
                ),
            ),
            ShimFunction::raw(
                Identifier::parse(&release_forwarder)?,
                format!(
                    "extern void {release_entry}(BoltFFINativeImageHandleCell *__context);\nstatic void {release_forwarder}(void *__context) {{\n    {release_entry}((BoltFFINativeImageHandleCell *)__context);\n}}"
                ),
            ),
            ShimFunction::raw(
                Identifier::parse(&call_address)?,
                format!("void *{call_address}(void) {{\n    return (void *){call_forwarder};\n}}"),
            ),
            ShimFunction::raw(
                Identifier::parse(&release_address)?,
                format!(
                    "void *{release_address}(void) {{\n    return (void *){release_forwarder};\n}}"
                ),
            ),
        ];

        Ok(Self {
            jni_call: registration.call().as_str().to_owned(),
            call_address,
            release_address,
            trampolines,
            shims,
        })
    }

    /// Returns the JNI C call symbol identifying this signature on a native method.
    pub fn jni_call(&self) -> &str {
        &self.jni_call
    }

    /// Returns the glue accessor for the call forwarder's address.
    pub fn call_address(&self) -> &str {
        &self.call_address
    }

    /// Returns the glue accessor for the release forwarder's address.
    pub fn release_address(&self) -> &str {
        &self.release_address
    }

    /// Returns the `@CEntryPoint` trampolines.
    pub fn trampolines(&self) -> &[Trampoline] {
        &self.trampolines
    }

    /// Returns the glue forwarders and address accessors.
    pub fn shims(&self) -> &[ShimFunction] {
        &self.shims
    }
}

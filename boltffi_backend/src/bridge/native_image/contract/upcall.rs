//! Upcalls: Rust calling Java-implemented callbacks through vtables.
//!
//! The C bridge gives every callback trait a vtable whose slots take the
//! callback handle as `uint64_t` first. In Native Image mode that handle is a
//! pointer to an unmanaged `HandleCell` holding the isolate and the Java
//! registry token. Each slot is a C shim in the glue file that forwards to an
//! exported `@CEntryPoint` whose prologue reads the isolate from the cell and
//! attaches the calling thread. The glue owns the vtables and registers them
//! with Rust once, exactly as the JNI glue does.
//!
//! Struct-by-value slot parameters and returns are pointerized on the way into
//! Java, the same way downcall shims pointerize C exports.

use crate::{
    bridge::{
        c::{self, Identifier},
        jni::{CallbackMethod, CallbackRegistration, JniType},
    },
    core::{Error, Result},
};

use super::{
    super::NATIVE_IMAGE_BRIDGE,
    CompletionUpcall, JavaCType, ShimFunction,
    asynchronous::{completion_names, payload_suffix},
    shim::{Pointerized, c_declaration},
};

/// One `@CEntryPoint` trampoline backing a vtable slot.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Trampoline {
    /// Exported entry point symbol the C shim calls.
    entry_symbol: String,
    /// Java method name inside `BoltFFINativeImage`.
    java_name: String,
    /// Java parameter declarations after the leading `HandleCell`.
    java_params: String,
    /// Java return type.
    java_returns: JavaCType,
    /// Java statements.
    body: Vec<String>,
}

impl Trampoline {
    /// Creates one `@CEntryPoint` trampoline.
    pub(super) fn new(
        entry_symbol: String,
        java_name: String,
        java_params: String,
        java_returns: JavaCType,
        body: Vec<String>,
    ) -> Self {
        Self {
            entry_symbol,
            java_name,
            java_params,
            java_returns,
            body,
        }
    }

    /// Returns the exported symbol name.
    pub fn entry_symbol(&self) -> &str {
        &self.entry_symbol
    }

    /// Returns the Java method name.
    pub fn java_name(&self) -> &str {
        &self.java_name
    }

    /// Returns the Java parameter declarations after the handle cell.
    pub fn java_params(&self) -> &str {
        &self.java_params
    }

    /// Returns the Java return type.
    pub const fn java_returns(&self) -> JavaCType {
        self.java_returns
    }

    /// Returns the Java statements.
    pub fn body(&self) -> &[String] {
        &self.body
    }
}

/// Everything one Java-implemented callback trait needs in Native Image mode.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct CallbackUpcall {
    /// C vtable typedef.
    vtable_type: String,
    /// C static vtable instance in the glue.
    vtable: String,
    /// C bridge registration function.
    register: String,
    /// `@CFunction` symbol of the pointer-form `create_handle` shim.
    create_handle_lib: String,
    /// Java class holding the registry and static dispatchers.
    dispatch_class: String,
    /// Java helper that converts a host token into a `BoltFFICallbackHandle`.
    parameter_helper: String,
    trampolines: Vec<Trampoline>,
    shims: Vec<ShimFunction>,
}

impl CallbackUpcall {
    /// Builds trampolines and slot shims for one callback registration.
    pub fn new(
        registration: &CallbackRegistration,
        callback: &c::Callback,
        create_handle_lib: &str,
        completions: &[CompletionUpcall],
    ) -> Result<Self> {
        let stem = registration.vtable_type().as_str();
        let dispatch_class = registration.class().class_name().to_owned();
        let mut trampolines = Vec::new();
        let mut shims = Vec::new();
        let fields = callback.vtable().fields();
        let (free_field, clone_field) = match fields {
            [free, clone, ..] => (free.name(), clone.name()),
            _ => {
                return Err(Error::BrokenBridgeContract {
                    bridge: NATIVE_IMAGE_BRIDGE,
                    invariant: "callback vtable starts with free and clone slots",
                });
            }
        };

        // free(uint64_t handle)
        let free_fn = registration.free().as_str().to_owned();
        let free_entry = format!("boltffi_ni_java_{free_fn}");
        trampolines.push(Trampoline {
            entry_symbol: free_entry.clone(),
            java_name: free_fn.clone(),
            java_params: String::new(),
            java_returns: JavaCType::Void,
            body: vec![
                "try {".to_owned(),
                format!("    {dispatch_class}.free(__cell.token());"),
                "} catch (Throwable __failure) {".to_owned(),
                "    // JNI glue clears the exception; nothing to report to Rust here.".to_owned(),
                "}".to_owned(),
                "UnmanagedMemory.free(__cell);".to_owned(),
            ],
        });
        shims.push(ShimFunction::raw(
            Identifier::parse(&free_fn)?,
            format!(
                "extern void {free_entry}(BoltFFINativeImageHandleCell *handle);\n\
                 static void {free_fn}(uint64_t handle) {{\n    {free_entry}((BoltFFINativeImageHandleCell *)(uintptr_t)handle);\n}}"
            ),
        ));

        // uint64_t clone(uint64_t handle)
        let clone_fn = registration.clone_callback().as_str().to_owned();
        let clone_entry = format!("boltffi_ni_java_{clone_fn}");
        trampolines.push(Trampoline {
            entry_symbol: clone_entry.clone(),
            java_name: clone_fn.clone(),
            java_params: String::new(),
            java_returns: JavaCType::Long,
            body: vec![
                "try {".to_owned(),
                format!("    long __token = {dispatch_class}.clone(__cell.token());"),
                "    return __token == 0L ? 0L : newHandleCell(__token);".to_owned(),
                "} catch (Throwable __failure) {".to_owned(),
                "    return 0L;".to_owned(),
                "}".to_owned(),
            ],
        });
        shims.push(ShimFunction::raw(
            Identifier::parse(&clone_fn)?,
            format!(
                "extern int64_t {clone_entry}(BoltFFINativeImageHandleCell *handle);\n\
                 static uint64_t {clone_fn}(uint64_t handle) {{\n    return (uint64_t){clone_entry}((BoltFFINativeImageHandleCell *)(uintptr_t)handle);\n}}"
            ),
        ));

        let mut vtable_init = vec![
            format!(".{free_field} = {free_fn}"),
            format!(".{clone_field} = {clone_fn}"),
        ];
        for (method, slot) in registration.methods().iter().zip(callback.methods()) {
            let (trampoline, shim) = Self::slot(method, slot, &dispatch_class, completions)?;
            vtable_init.push(format!(
                ".{} = {}",
                slot.name().as_str(),
                method.function().as_str()
            ));
            trampolines.push(trampoline);
            shims.push(shim);
        }
        let vtable = registration.vtable().as_str().to_owned();
        shims.push(ShimFunction::raw(
            Identifier::parse(&vtable)?,
            format!(
                "static {stem} {vtable} = {{\n    {}\n}};",
                vtable_init.join(",\n    ")
            ),
        ));
        Ok(Self {
            vtable_type: stem.to_owned(),
            vtable,
            register: callback.register().name().to_owned(),
            create_handle_lib: create_handle_lib.to_owned(),
            parameter_helper: format!("{stem}_parameter"),
            dispatch_class,
            trampolines,
            shims,
        })
    }

    fn slot(
        method: &CallbackMethod,
        slot: &c::CallbackSlot,
        dispatch_class: &str,
        completions: &[CompletionUpcall],
    ) -> Result<(Trampoline, ShimFunction)> {
        if method.returns_closure() {
            return Err(unsupported("callback method returning a closure"));
        }
        let function = method.function().as_str().to_owned();
        let entry = format!("boltffi_ni_java_{function}");
        // Slot parameters after the leading handle.
        let params = &slot.parameters()[1..];
        let pointerized = Pointerized::new(params, slot.returns())?;
        let record_type = |name: &str| -> Option<String> {
            params
                .iter()
                .find(|parameter| parameter.name() == name)
                .and_then(|parameter| match parameter.ty() {
                    c::Type::DirectRecord(id) => Some(id.as_str().to_owned()),
                    _ => None,
                })
        };

        let mut setup = Vec::new();
        let mut arguments = vec!["__cell.token()".to_owned()];
        let mut failure_completion: Option<String> = None;
        let handle_name = slot.parameters()[0].name();
        for (position, argument) in method.jvm_arguments().iter().enumerate() {
            if let Some((parameter, _)) = argument.value() {
                if position == 0 && parameter.name().as_str() == handle_name {
                    // The leading uint64 handle is replaced by the cell's Java token.
                    continue;
                }
                arguments.push(parameter.name().as_str().to_owned());
            } else if let Some(out) = argument.success_out() {
                arguments.push(format!("{}.rawValue()", out.name().as_str()));
            } else if let Some(bytes) = argument.bytes() {
                setup.push(format!(
                    "byte[] {} = copyBytes(WordFactory.pointer({}.rawValue()), {});",
                    bytes.name().as_str(),
                    bytes.pointer().as_str(),
                    bytes.length().as_str()
                ));
                arguments.push(bytes.name().as_str().to_owned());
            } else if let Some(vector) = argument.direct_vector() {
                setup.push(format!(
                    "{}[] {} = {}(WordFactory.pointer({}.rawValue()), {});",
                    JavaCType::from_jni(vector.jni_type()).java(),
                    vector.array().as_str(),
                    copy_helper(vector.jni_type()),
                    vector.pointer().as_str(),
                    vector.length().as_str()
                ));
                arguments.push(vector.array().as_str().to_owned());
            } else if let Some(record) = argument.record() {
                let c_type = record_type(record.parameter().as_str()).ok_or(
                    Error::BrokenBridgeContract {
                        bridge: NATIVE_IMAGE_BRIDGE,
                        invariant: "callback record argument names a direct record parameter",
                    },
                )?;
                setup.push(format!(
                    "byte[] {} = copyRecord({}, SizeOf.get({c_type}Struct.class));",
                    record.array().as_str(),
                    record.parameter().as_str()
                ));
                arguments.push(record.array().as_str().to_owned());
            } else if let Some(handle) = argument.callback_handle() {
                // The slot receives the handle by value; the shim passes its address.
                setup.push(format!(
                    "long {} = storeOwnedHandle((CallbackHandle) {});",
                    handle.handle().as_str(),
                    handle.parameter().as_str()
                ));
                arguments.push(handle.handle().as_str().to_owned());
            } else if argument.closure_handle().is_some() {
                return Err(unsupported("closure argument in callback method"));
            } else if let Some(completion) = argument.completion() {
                let (callback, context) =
                    completion_names(slot).ok_or(Error::BrokenBridgeContract {
                        bridge: NATIVE_IMAGE_BRIDGE,
                        invariant: "asynchronous slot has a completion parameter group",
                    })?;
                let suffix = payload_suffix(completion.payload());
                failure_completion = completions
                    .iter()
                    .find(|upcall| upcall.suffix() == suffix)
                    .map(|upcall| format!("Lib.{}({callback}, {context});", upcall.failure_lib()));
                arguments.push(format!("{callback}.rawValue()"));
                arguments.push(format!("{context}.rawValue()"));
            } else {
                return Err(Error::BrokenBridgeContract {
                    bridge: NATIVE_IMAGE_BRIDGE,
                    invariant: "callback argument has a known kind",
                });
            }
        }
        let call = format!(
            "{dispatch_class}.{}({})",
            method.method().as_str(),
            arguments.join(", ")
        );

        let body = match (slot.is_asynchronous(), failure_completion) {
            // An async slot returns before the work finishes; a Java failure has
            // to be reported by completing the future Rust is already waiting on.
            (true, Some(failure)) => std::iter::once("try {".to_owned())
                .chain(setup.iter().map(|line| format!("    {line}")))
                .chain([
                    format!("    {call};"),
                    "} catch (Throwable __failure) {".to_owned(),
                    format!("    {failure}"),
                    "}".to_owned(),
                ])
                .collect(),
            (true, None) => return Err(unsupported("asynchronous callback completion")),
            (false, _) => trampoline_body(&call, &setup, &pointerized, slot.returns(), None)?,
        };

        let trampoline = Trampoline {
            entry_symbol: entry.clone(),
            java_name: function.clone(),
            java_params: pointerized.java_declarations(),
            java_returns: pointerized.returns,
            body,
        };

        // C shim: original slot signature -> pointer-form Java entry point.
        let slot_params = std::iter::once(Ok("uint64_t handle".to_owned()))
            .chain(
                params
                    .iter()
                    .map(|parameter| c_declaration(parameter.ty(), parameter.name())),
            )
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        let forwarded =
            std::iter::once("(BoltFFINativeImageHandleCell *)(uintptr_t)handle".to_owned())
                .chain(params.iter().map(|parameter| {
                    match JavaCType::is_by_value_struct(parameter.ty()) {
                        true => format!("&{}", parameter.name()),
                        false => parameter.name().to_owned(),
                    }
                }))
                .chain(pointerized.out_param.then(|| "&__out".to_owned()))
                .collect::<Vec<_>>()
                .join(", ");
        let entry_params = std::iter::once("BoltFFINativeImageHandleCell *handle".to_owned())
            .chain(std::iter::once(pointerized.c_declarations()?).filter(|s| !s.is_empty()))
            .collect::<Vec<_>>()
            .join(", ");
        let slot_return = c::TypeFragment::anonymous(slot.returns())?.to_string();
        let entry_call = format!("{entry}({forwarded})");
        let shim_body = if pointerized.status_return {
            format!("return (FfiStatus){{ .code = {entry_call} }};")
        } else if pointerized.out_param {
            format!(
                "{slot_return} __out;\n    memset(&__out, 0, sizeof(__out));\n    {entry_call};\n    return __out;"
            )
        } else if pointerized.returns.is_void() {
            format!("{entry_call};")
        } else {
            format!("return {entry_call};")
        };
        let shim = ShimFunction::raw(
            Identifier::parse(&function)?,
            format!(
                "extern {} {entry}({entry_params});\nstatic {slot_return} {function}({slot_params}) {{\n    {shim_body}\n}}",
                pointerized.c_return()?
            ),
        );
        Ok((trampoline, shim))
    }

    /// Returns the C vtable typedef name.
    pub fn vtable_type(&self) -> &str {
        &self.vtable_type
    }

    /// Returns the glue's static vtable instance name.
    pub fn vtable(&self) -> &str {
        &self.vtable
    }

    /// Returns the C bridge registration function.
    pub fn register(&self) -> &str {
        &self.register
    }

    /// Returns the `Lib` symbol of the pointer-form `create_handle`.
    pub fn create_handle_lib(&self) -> &str {
        &self.create_handle_lib
    }

    /// Returns the Java class with the token registry and static dispatchers.
    pub fn dispatch_class(&self) -> &str {
        &self.dispatch_class
    }

    /// Returns the Java helper converting a host token into a callback handle.
    pub fn parameter_helper(&self) -> &str {
        &self.parameter_helper
    }

    /// Returns the `@CEntryPoint` trampolines.
    pub fn trampolines(&self) -> &[Trampoline] {
        &self.trampolines
    }

    /// Returns slot shims and the vtable definition, in emission order.
    pub fn shims(&self) -> &[ShimFunction] {
        &self.shims
    }
}

/// Renders the guarded Java statements of one upcall trampoline.
///
/// Every upcall body has the same shape: zero any output storage, run the
/// argument conversions, call the host dispatcher, and swallow a Java failure
/// into the C failure value, because there is no exception channel into Rust.
pub(super) fn trampoline_body(
    call: &str,
    setup: &[String],
    pointerized: &Pointerized,
    returns: &c::Type,
    callback_handle_helper: Option<&str>,
) -> Result<Vec<String>> {
    let mut body = Vec::new();
    let failure;
    if pointerized.out_param {
        match returns {
            c::Type::Buffer => {
                body.push("zeroBuffer((FfiBuf) __out);".to_owned());
                body.push("try {".to_owned());
                body.extend(setup.iter().map(|line| format!("    {line}")));
                body.push(format!("    byte[] __value = {call};"));
                body.push("    writeBuffer((FfiBuf) __out, __value);".to_owned());
            }
            c::Type::DirectRecord(id) => {
                body.push(format!(
                    "zeroMemory(__out, SizeOf.get({}Struct.class));",
                    id.as_str()
                ));
                body.push("try {".to_owned());
                body.extend(setup.iter().map(|line| format!("    {line}")));
                body.push(format!("    byte[] __value = {call};"));
                body.push(format!(
                    "    writeRecord(__out, __value, SizeOf.get({}Struct.class));",
                    id.as_str()
                ));
            }
            c::Type::CallbackHandle(_) => {
                let helper = callback_handle_helper
                    .ok_or_else(|| unsupported("callback handle returned by this upcall"))?;
                body.push("zeroCallbackHandle((CallbackHandle) __out);".to_owned());
                body.push("try {".to_owned());
                body.extend(setup.iter().map(|line| format!("    {line}")));
                body.push(format!("    long __token = {call};"));
                body.push(format!("    {helper}(__token, (CallbackHandle) __out);"));
            }
            _ => return Err(unsupported("upcall returning this struct")),
        }
        failure = None;
    } else if pointerized.status_return {
        body.push("try {".to_owned());
        body.extend(setup.iter().map(|line| format!("    {line}")));
        body.push(format!("    return {call};"));
        failure = Some("1".to_owned());
    } else if pointerized.returns.is_void() {
        body.push("try {".to_owned());
        body.extend(setup.iter().map(|line| format!("    {line}")));
        body.push(format!("    {call};"));
        failure = None;
    } else {
        body.push("try {".to_owned());
        body.extend(setup.iter().map(|line| format!("    {line}")));
        body.push(format!("    return {call};"));
        failure = Some(match pointerized.returns {
            JavaCType::Boolean => "false".to_owned(),
            JavaCType::Float => "0.0f".to_owned(),
            JavaCType::Double => "0.0".to_owned(),
            _ => "0".to_owned(),
        });
    }
    body.push("} catch (Throwable __failure) {".to_owned());
    match &failure {
        Some(value) => body.push(format!("    return {value};")),
        None => body.push("    // Failure leaves the zeroed output in place.".to_owned()),
    }
    body.push("}".to_owned());
    Ok(body)
}

pub(super) fn copy_helper(ty: JniType) -> &'static str {
    match ty {
        JniType::Boolean => "copyBooleans",
        JniType::Byte => "copyBytes",
        JniType::Short => "copyShorts",
        JniType::Int => "copyInts",
        JniType::Long => "copyLongs",
        JniType::Float => "copyFloats",
        JniType::Double => "copyDoubles",
    }
}

pub(super) fn unsupported(shape: &'static str) -> Error {
    Error::UnsupportedTarget {
        target: NATIVE_IMAGE_BRIDGE,
        shape,
    }
}

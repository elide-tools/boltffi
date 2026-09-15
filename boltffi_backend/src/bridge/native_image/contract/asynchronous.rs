//! Async plumbing: poll continuations and async callback completions.
//!
//! Two things cross the boundary after a call has already returned. A poll
//! continuation is a C function pointer Rust invokes when a future becomes
//! ready; in Native Image mode it is a glue forwarder that enters the isolate
//! recorded in a one-shot `HandleCell` holding the host's callback-data token.
//! A callback completion is the reverse: Java finishes an async callback by
//! calling a C function pointer Rust handed it, which takes an `FfiStatus` and
//! payload by value, so the call goes through a pointer-ABI glue shim.

use crate::{
    bridge::{
        c::{self, Identifier},
        jni::{
            CallbackCompletionInvoker, CallbackCompletionPayload, CallbackCompletionPayloadValue,
        },
    },
    core::Result,
};

use super::{JavaCType, ShimFunction, Trampoline, Wrapper, WrapperParameter, upcall::unsupported};

/// Exported entry point behind the shared poll continuation.
pub const CONTINUATION_ENTRY: &str = "boltffi_ni_java_continuation";
/// Glue accessor returning the shared continuation forwarder's address.
pub const CONTINUATION_ADDRESS: &str = "boltffi_ni_continuation_callback";
/// Java method name of the continuation trampoline.
pub const CONTINUATION_METHOD: &str = "boltffi_ni_continuation";
/// Host static the continuation trampoline resumes.
pub const CONTINUATION_DISPATCH: &str = "boltffiFutureContinuationCallback";

/// Builds the one-shot continuation trampoline and its glue forwarder.
///
/// Every poll continuation shares one C signature, `(uint64_t callback_data,
/// int8_t poll_result)`, so one entry point serves futures and streams alike.
/// The cell is freed before dispatch because a continuation fires exactly once.
pub(super) fn continuation(host_class: &str) -> Result<(Trampoline, Vec<ShimFunction>)> {
    let trampoline = Trampoline::new(
        CONTINUATION_ENTRY.to_owned(),
        CONTINUATION_METHOD.to_owned(),
        "byte pollResult".to_owned(),
        JavaCType::Void,
        vec![
            "long __token = __cell.token();".to_owned(),
            "UnmanagedMemory.free(__cell);".to_owned(),
            "try {".to_owned(),
            format!("    {host_class}.{CONTINUATION_DISPATCH}(__token, pollResult);"),
            "} catch (Throwable __failure) {".to_owned(),
            "    // A resumed future owns its own failure reporting.".to_owned(),
            "}".to_owned(),
        ],
    );
    let forwarder = "boltffi_ni_continuation_shim";
    let shims = vec![
        ShimFunction::raw(
            Identifier::parse(forwarder)?,
            format!(
                "extern void {CONTINUATION_ENTRY}(BoltFFINativeImageHandleCell *cell, int8_t poll_result);\n\
                 static void {forwarder}(uint64_t callback_data, int8_t poll_result) {{\n    \
                 {CONTINUATION_ENTRY}((BoltFFINativeImageHandleCell *)(uintptr_t)callback_data, poll_result);\n}}"
            ),
        ),
        ShimFunction::raw(
            Identifier::parse(CONTINUATION_ADDRESS)?,
            format!("void *{CONTINUATION_ADDRESS}(void) {{\n    return (void *){forwarder};\n}}"),
        ),
    ];
    Ok((trampoline, shims))
}

/// One async callback completion function, spelled for Java.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct CompletionCall {
    lib_name: String,
    wrapper: Wrapper,
    shim: ShimFunction,
    lib_params: Vec<(String, JavaCType)>,
}

impl CompletionCall {
    /// Returns the glue symbol the Java wrapper calls.
    pub fn lib_name(&self) -> &str {
        &self.lib_name
    }

    /// Returns the `@CFunction` parameters, as `(name, Java type)` pairs.
    pub fn lib_params(&self) -> &[(String, JavaCType)] {
        &self.lib_params
    }

    /// Returns the Java wrapper with the JNI `Native` signature.
    pub fn wrapper(&self) -> &Wrapper {
        &self.wrapper
    }

    /// Returns the glue shim that calls the completion function pointer.
    pub fn shim(&self) -> &ShimFunction {
        &self.shim
    }
}

/// Everything one async callback completion invoker needs in Native Image mode.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct CompletionUpcall {
    /// Payload suffix shared with the JNI invoker, used to match a callback slot.
    suffix: String,
    /// Glue symbol completing with failure, called from a failed async trampoline.
    failure_lib: String,
    calls: Vec<CompletionCall>,
}

impl CompletionUpcall {
    /// Builds the success, failure, and error completion calls for one payload shape.
    pub(super) fn new(invoker: &CallbackCompletionInvoker) -> Result<Self> {
        let payload = invoker.payload();
        if payload.is_some_and(|payload| {
            matches!(
                payload.value(),
                CallbackCompletionPayloadValue::CallbackHandle
            )
        }) {
            return Err(unsupported("callback handle completion payload"));
        }
        let suffix = payload.map_or_else(|| "Void".to_owned(), |value| value.suffix().to_owned());
        let failure_lib = format!("boltffi_ni_{}", invoker.failure_method().as_str());
        let calls = [
            (invoker.success_method().as_str(), Some(0), payload),
            (invoker.failure_method().as_str(), Some(1), None),
        ]
        .into_iter()
        .chain(
            invoker
                .error_method()
                .map(|method| (method.as_str(), Some(1), payload)),
        )
        .map(|(method, code, payload)| Self::call(method, code.unwrap_or(0), payload))
        .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            suffix,
            failure_lib,
            calls,
        })
    }

    fn call(
        method: &str,
        code: i32,
        payload: Option<&CallbackCompletionPayload>,
    ) -> Result<CompletionCall> {
        let lib_name = format!("boltffi_ni_{method}");
        let c_payload = payload.map(|payload| payload.c_type().to_string());
        let complete_params = format!(
            "(void *, FfiStatus{})",
            c_payload
                .as_ref()
                .map_or_else(String::new, |ty| format!(", {ty}"))
        );

        let mut params = vec![
            WrapperParameter::named("callback", "long"),
            WrapperParameter::named("context", "long"),
        ];
        let mut lib_params = vec![
            ("callback".to_owned(), JavaCType::Pointer),
            ("context".to_owned(), JavaCType::Pointer),
        ];
        let mut shim_params = vec!["void *__callback".to_owned(), "void *__context".to_owned()];
        let mut shim_arguments = vec![
            "__context".to_owned(),
            format!("(FfiStatus){{ .code = {code} }}"),
        ];
        let prologue = [
            "org.graalvm.word.PointerBase __callback = WordFactory.pointer(callback);".to_owned(),
            "org.graalvm.word.PointerBase __context = WordFactory.pointer(context);".to_owned(),
        ];
        let mut body = prologue.to_vec();

        match payload.map(CallbackCompletionPayload::value) {
            None => body.push(format!("Lib.{lib_name}(__callback, __context);")),
            Some(CallbackCompletionPayloadValue::Bytes) => {
                params.push(WrapperParameter::named("result", "byte[]"));
                lib_params.push(("payload".to_owned(), JavaCType::Pointer));
                shim_params.push("const FfiBuf_u8 *__payload".to_owned());
                shim_arguments.push("*__payload".to_owned());
                body.push("FfiBuf __payload = StackValue.get(FfiBuf.class);".to_owned());
                body.push("zeroBuffer(__payload);".to_owned());
                body.push("writeBuffer(__payload, result);".to_owned());
                body.push(format!("Lib.{lib_name}(__callback, __context, __payload);"));
            }
            Some(CallbackCompletionPayloadValue::Record) => {
                let c_type = c_payload.clone().unwrap_or_default();
                params.push(WrapperParameter::named("result", "byte[]"));
                lib_params.push(("payload".to_owned(), JavaCType::Pointer));
                shim_params.push(format!("const {c_type} *__payload"));
                shim_arguments.push("*__payload".to_owned());
                body.push(format!(
                    "checkRecordLength(result, SizeOf.get({c_type}Struct.class));"
                ));
                body.push(
                    "try (PinnedObject __pin_payload = PinnedObject.create(result)) {".to_owned(),
                );
                body.push(format!(
                    "    Lib.{lib_name}(__callback, __context, __pin_payload.addressOfArrayElement(0));"
                ));
                body.push("}".to_owned());
            }
            Some(CallbackCompletionPayloadValue::Scalar(jni_type)) => {
                let java = JavaCType::from_jni(jni_type);
                params.push(WrapperParameter::named("result", java.java()));
                lib_params.push(("payload".to_owned(), java));
                shim_params.push(format!(
                    "{} __payload",
                    c_payload.clone().unwrap_or_default()
                ));
                shim_arguments.push("__payload".to_owned());
                body.push(format!("Lib.{lib_name}(__callback, __context, result);"));
            }
            Some(CallbackCompletionPayloadValue::CallbackHandle) => {
                return Err(unsupported("callback handle completion payload"));
            }
        }

        let shim = ShimFunction::raw(
            Identifier::parse(&lib_name)?,
            format!(
                "void {lib_name}({}) {{\n    void (*__complete){complete_params} = (void (*){complete_params})__callback;\n    __complete({});\n}}",
                shim_params.join(", "),
                shim_arguments.join(", ")
            ),
        );
        Ok(CompletionCall {
            lib_name,
            wrapper: Wrapper::raw(method.to_owned(), params, "void".to_owned(), body),
            shim,
            lib_params,
        })
    }

    /// Returns the payload suffix shared with the JNI completion invoker.
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// Returns the glue symbol that completes an async callback with failure.
    pub fn failure_lib(&self) -> &str {
        &self.failure_lib
    }

    /// Returns each completion call (success, failure, and optional error).
    pub fn calls(&self) -> &[CompletionCall] {
        &self.calls
    }
}

/// Returns the payload suffix a callback completion group uses.
pub(super) fn payload_suffix(payload: Option<&CallbackCompletionPayload>) -> String {
    payload.map_or_else(|| "Void".to_owned(), |value| value.suffix().to_owned())
}

/// Returns the C parameter names carrying a completion group's callback and context.
pub(super) fn completion_names(slot: &c::CallbackSlot) -> Option<(String, String)> {
    slot.parameter_groups()
        .iter()
        .find_map(|group| match group {
            c::ParameterGroup::CallbackCompletion(completion) => Some((
                slot.parameters()[completion.callback().position()]
                    .name()
                    .to_owned(),
                slot.parameters()[completion.context().position()]
                    .name()
                    .to_owned(),
            )),
            _ => None,
        })
}

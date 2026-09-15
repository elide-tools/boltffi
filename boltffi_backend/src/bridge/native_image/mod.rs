//! GraalVM Native Image bridge layered above the C ABI bridge.
//!
//! Like the JNI bridge, this layer adapts one `CBridgeContract` for the JVM.
//! Unlike JNI, it emits Java, not C: `@CFunction` declarations that call the C
//! ABI directly, Java wrapper methods with the same signatures the JNI `Native`
//! class exposes, and `@CEntryPoint` trampolines for upcalls. A small
//! generated C shim covers by-value struct shapes that `@CFunction` cannot
//! express, and is compiled into the same static library as the Rust core.
//!
//! The JVM-facing facts (Java parameter kinds, return shapes, callback
//! registrations) come from the shared `JniBridgeContract` builder, so host
//! renderers see one contract type regardless of binding.

mod bridge;
mod contract;
mod template;

const NATIVE_IMAGE_BRIDGE: &str = "native-image";

pub use bridge::{NativeImageBridge, TransitionPolicy};
pub use contract::{
    CallbackUpcall, ClosureUpcall, CompletionCall, CompletionUpcall, Downcall, JavaCType,
    LibParameter, NativeImageContract, ShimFunction, Trampoline, Transition, Wrapper,
    WrapperParameter,
};

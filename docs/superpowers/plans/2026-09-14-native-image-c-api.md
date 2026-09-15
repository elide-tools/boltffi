# Native Image C API Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the Java target emit bindings that call the BoltFFI C ABI through the GraalVM Native Image C API instead of JNI, statically linked, with upcalls from foreign Rust threads.

**Architecture:** A new `NativeImageBridge` layers above `CBridge` beside the existing `JniBridge`, reusing the `JniBridgeContract` builder for JVM-facing facts. It renders `BoltFFINativeImage.java` (`@CFunction` declarations, Java wrappers with JNI-identical signatures, `@CEntryPoint` trampolines) plus a pointer-ABI C shim `native_image_glue.c` for by-value structs. `JavaHost` gains a `JvmBinding` selector; in Native Image mode its `Native` class forwards to `BoltFFINativeImage` and the loader is empty.

**Tech Stack:** Rust (Askama templates), GraalVM 25 `org.graalvm.nativeimage` API plus `com.oracle.svm.guest.staging.c.function` internals, Rust `staticlib`, C11 shim.

**Spec:** `docs/superpowers/specs/2026-09-14-native-image-c-api-design.md`

## Status (2026-09-14)

- Task 1 done (`14e79fc3`), Task 2 done (`eaa84689`), Task 3 done (`70e9c756`),
  Task 4a done (`975962aa`): Java-implemented callbacks with sync methods,
  free/clone, callback parameters, Rust-owned handle returns, lifecycle and
  success-out writer wrappers.
- Task 3 needed no host `Transport` change: the spike proved
  `MemorySegment.ofBuffer(buffer).address()` works in Native Image, so the
  host's existing direct-buffer transport is kept and `directAddress()` reads it.
- Design change from the plan: the glue owns the vtables and registers them
  through `boltffi_ni_register_callbacks()`; slots are C shims that forward to
  exported `@CEntryPoint`s (`boltffi_ni_java_*`). This pointerizes struct
  slot arguments the same way downcall shims do.
- Task 4b done (`84b8b10c`): Rust-owned callback handle methods dispatch
  through a `boltffi_ni_invoke_<vtable>_<slot>` glue invoker that casts the
  stored vtable; callback-handle arguments inside Java-implemented callback
  methods copy the incoming handle with `storeOwnedHandle`.
- Task 4c done in part (`2216e3c4`): JVM-owned closures get a call and a
  release `@CEntryPoint` per signature, a `HandleCell` context, static glue
  forwarders, and two `void *` glue accessors the downcall wrapper reads to
  build the `(call, context, release)` triple.
- Task 4d done (`b881344e`): one shared poll continuation entry point behind a
  one-shot `HandleCell`, `CompletionStatusOut` as stack `CIntPointer` storage,
  and async callback methods completing through pointer-ABI completion shims.
- Task 4e done (`24b74688`): stream protocol methods render from
  `jvm.streams()`, and the direct batch pins a Java byte array across the call.
- Still unsupported, each failing at generation time: Rust-owned closure
  handles (closure arguments in callback methods, `ClosureReturn` groups, and
  the `ClosureRegistration` handle helpers), callback-handle completion
  payloads, async callback handle methods, and closures returning callback
  handles.
- Remaining: the closure-handle shapes above, 5 packaging, 6 runtime test,
  7 docs.

## Global Constraints

- Native Image only. Generated Java must not be expected to run on HotSpot.
- Static linking only: `@CLibrary(value = <lib>, requireStatic = true)`, no `System.loadLibrary`.
- Public Java API (records, classes, enums, callbacks, `Demo.java`) is byte-identical between JNI and Native Image modes. Only `Native`'s member bodies and the loader change.
- Unsupported shapes fail at generation time with `Error::UnsupportedTarget`, never with a runtime stub.
- Internal GraalVM classes are referenced only from the generated `BoltFFINativeImage` class so a package rename is one template change. Exact names for GraalVM 25: `com.oracle.svm.guest.staging.c.function.CEntryPointOptions`, `com.oracle.svm.guest.staging.c.function.CEntryPointActions`, `com.oracle.svm.shared.Uninterruptible`.
- `--add-exports` flags that generated code needs, verbatim:
  `--add-exports org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c.function=ALL-UNNAMED`
  `--add-exports org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c=ALL-UNNAMED`
  `--add-exports org.graalvm.nativeimage.shared/com.oracle.svm.shared=ALL-UNNAMED`
- Header include path is supplied by `Directives.getOptions()` returning `-I` + `System.getProperty("boltffi.include")`. `-H:CLibraryPath` is for libraries only.
- All existing JNI snapshot and runtime tests stay green: `cargo test -p boltffi_backend`.
- Commit after every task with a conventional-commit message ending in `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

---

## File Structure

Create:
- `boltffi_backend/src/bridge/native_image/mod.rs` — module docs, re-exports.
- `boltffi_backend/src/bridge/native_image/bridge.rs` — `NativeImageBridge` (`BridgeBackend` impl).
- `boltffi_backend/src/bridge/native_image/contract/mod.rs` — `NativeImageContract` view built from `JniBridgeContract`: downcall plans, shim functions, trampolines.
- `boltffi_backend/src/bridge/native_image/contract/downcall.rs` — `Downcall` (one `Lib` declaration + one wrapper): parameter lowering, transition, shim decision.
- `boltffi_backend/src/bridge/native_image/contract/shim.rs` — `ShimFunction`: pointer-ABI C wrapper for by-value shapes.
- `boltffi_backend/src/bridge/native_image/contract/java_type.rs` — `JavaCType`: C type → `@CFunction` Java parameter/return spelling.
- `boltffi_backend/src/bridge/native_image/contract/upcall.rs` — `Trampoline`, `HandleCell` facts for callbacks, closures, streams, completions.
- `boltffi_backend/src/bridge/native_image/template/mod.rs` — Askama wrappers.
- `boltffi_backend/templates/bridge/native_image/native_image.java` — top-level Java file.
- `boltffi_backend/templates/bridge/native_image/support.java` — `Directives`, `HandleCell`, `AttachPrologue`, buffer helpers.
- `boltffi_backend/templates/bridge/native_image/lib.java` — `@CFunction` declarations.
- `boltffi_backend/templates/bridge/native_image/wrapper.java` — one wrapper method.
- `boltffi_backend/templates/bridge/native_image/trampoline.java` — one `@CEntryPoint`.
- `boltffi_backend/templates/bridge/native_image/glue.c` — shim source.
- `boltffi_backend/tests/native_image.rs` — snapshot tests.
- `boltffi_backend/tests/native_image_runtime.rs` — gated end-to-end image test.
- `boltffi_cli/src/pack/java/native_image.rs` — staticlib + shim + `native-image.properties`.
- `docs/src/content/docs/native-image.mdx` — user docs.

Modify:
- `boltffi_backend/src/core/capabilities.rs:42-53` — add `BridgeCapability::NativeImageC`.
- `boltffi_backend/src/bridge/mod.rs` — `pub mod native_image;`.
- `boltffi_backend/src/bridge/jni/contract/bridge/mod.rs`, `build.rs` — `BridgeOwner`, `for_native_image` constructor, record `Transport`.
- `boltffi_backend/src/bridge/jni/contract/record/parameter.rs` — `Transport` enum on `RecordParameter`.
- `boltffi_backend/src/target/java/mod.rs` — `JvmBinding`, `binding()` builder, target stack selection, capability requirement, name collision.
- `boltffi_backend/src/target/java/render/native.rs` + `templates/target/java/native_method.java` — forward body in Native Image mode.
- `boltffi_backend/src/target/java/render/record.rs` — transport-driven `toByteArray()`/`toDirectBuffer()`.
- `boltffi_backend/src/target/java/runtime/loader.rs` — empty loader for Native Image.
- `boltffi_backend/src/target/jvm/library.rs` — `DesktopLoader::None`.
- `boltffi_cli/src/config/targets/*.rs`, `boltffi_cli/src/pack/java/plan.rs` — `binding` option and mode dispatch.
- `BOLTFFI_TOML_SPEC.md` — document `binding`.

---

### Task 1: Capability, `JvmBinding`, and the host seam

**Files:**
- Modify: `boltffi_backend/src/core/capabilities.rs:42-53`
- Modify: `boltffi_backend/src/bridge/jni/contract/bridge/mod.rs`
- Modify: `boltffi_backend/src/bridge/jni/contract/bridge/build.rs`
- Modify: `boltffi_backend/src/target/java/mod.rs`
- Modify: `boltffi_backend/src/target/jvm/library.rs`
- Modify: `boltffi_backend/src/target/java/runtime/loader.rs`
- Modify: `boltffi_backend/src/target/java/render/native.rs`
- Modify: `boltffi_backend/templates/target/java/native_method.java`
- Test: `boltffi_backend/src/target/java/mod.rs` (unit), `boltffi_backend/tests/java.rs`

**Interfaces:**
- Produces `BridgeCapability::NativeImageC`.
- Produces `pub enum BridgeOwner { Host, NativeImage }` on `JniBridgeContract` with `pub const fn owner(&self) -> BridgeOwner` and `pub fn native_image_class(&self) -> Option<&JvmClassPath>`.
- Produces `JniBridgeContract::for_native_image(class: JvmClassPath, native_image_class: JvmClassPath, c_bridge: &CBridgeContract) -> Result<Self>` which sets capabilities `CAbi + NativeImageC` and `owner = NativeImage`. (`from_c_bridge` keeps `CAbi + Jni`, `owner = Host`.)
- Produces `pub enum JvmBinding { Jni, NativeImage }` in `target::java`, `JavaHost::binding(self, JvmBinding) -> Self`, `JavaHost::NATIVE_IMAGE_CLASS = "BoltFFINativeImage"`.
- Produces `DesktopLoader::None` in `target::jvm`.
- Host `Native` forwards: `static T name(params) { return BoltFFINativeImage.name(args); }` when `bridge.owner() == BridgeOwner::NativeImage`.

- [ ] **Step 1: Add the capability**

In `boltffi_backend/src/core/capabilities.rs` after `Napi,`:

```rust
    /// GraalVM Native Image C API surface (`@CFunction` downcalls, `@CEntryPoint` upcalls).
    NativeImageC,
```

Fix any exhaustive `match` on `BridgeCapability` the compiler reports (`cargo check -p boltffi_backend`).

- [ ] **Step 2: Write the failing owner test**

Append to `boltffi_backend/src/bridge/jni/contract/bridge/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bridge::c::CBridgeContract, core::BridgeCapability};
    use boltffi_ast::PackageInfo;
    use boltffi_binding::lower;

    fn c_bridge() -> CBridgeContract {
        let file = syn::parse_str("#[export] pub fn ping() {}").unwrap();
        let source = boltffi_scan::scan_file(file, PackageInfo::new("demo", None)).unwrap();
        let bindings = lower::<Native>(&source).unwrap();
        CBridgeContract::from_bindings(&bindings, FilePath::new("demo.h").unwrap()).unwrap()
    }

    #[test]
    fn jni_contract_is_host_owned() {
        let class = JvmClassPath::new("com.demo", "Native").unwrap();
        let contract = JniBridgeContract::from_c_bridge(
            class, FilePath::new("jni_glue.c").unwrap(), JniHeaderStyle::Quoted, &c_bridge()).unwrap();
        assert_eq!(contract.owner(), BridgeOwner::Host);
        assert!(contract.native_image_class().is_none());
        assert!(contract.capabilities().status(BridgeCapability::Jni).is_stable());
    }

    #[test]
    fn native_image_contract_is_bridge_owned() {
        let class = JvmClassPath::new("com.demo", "Native").unwrap();
        let ni = JvmClassPath::new("com.demo", "BoltFFINativeImage").unwrap();
        let contract = JniBridgeContract::for_native_image(class, ni.clone(), &c_bridge()).unwrap();
        assert_eq!(contract.owner(), BridgeOwner::NativeImage);
        assert_eq!(contract.native_image_class(), Some(&ni));
        assert!(contract.capabilities().status(BridgeCapability::NativeImageC).is_stable());
        assert!(!contract.capabilities().status(BridgeCapability::Jni).is_stable());
    }
}
```

Check `BridgeCapabilities::status` exists (`grep -n 'pub fn status' boltffi_backend/src/core/capabilities.rs`); if it is named differently, use that name.

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p boltffi_backend --lib bridge::jni::contract::bridge::tests`
Expected: compile error, `owner` and `for_native_image` not found.

- [ ] **Step 4: Implement `BridgeOwner`**

In `bridge/mod.rs` add fields and accessors:

```rust
/// Which layer renders the JVM `Native` owner class members.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum BridgeOwner {
    /// The host renders `static native` declarations; the bridge renders JNI C glue.
    Host,
    /// The bridge renders a Java class the host's `Native` forwards to.
    NativeImage,
}
```

Add `owner: BridgeOwner` and `native_image_class: Option<JvmClassPath>` to the struct, plus:

```rust
    /// Returns who renders `Native` member bodies.
    pub const fn owner(&self) -> BridgeOwner { self.owner }
    /// Returns the bridge-rendered Java class in Native Image mode.
    pub fn native_image_class(&self) -> Option<&JvmClassPath> { self.native_image_class.as_ref() }
```

In `build.rs`, refactor the body of `from_c_bridge` into a private `fn build(class, source_path, header_style, c_bridge, capabilities: BridgeCapabilities, owner: BridgeOwner, native_image_class: Option<JvmClassPath>) -> Result<Self>` and add:

```rust
    /// Builds the contract for the Native Image bridge (no JNI symbols; bridge renders Java).
    pub fn for_native_image(
        class: JvmClassPath,
        native_image_class: JvmClassPath,
        c_bridge: &c::CBridgeContract,
    ) -> Result<Self> {
        Self::build(
            class,
            FilePath::new("BoltFFINativeImage.java")?,
            JniHeaderStyle::SearchPath,
            c_bridge,
            BridgeCapabilities::new()
                .stable(BridgeCapability::CAbi)
                .stable(BridgeCapability::NativeImageC),
            BridgeOwner::NativeImage,
            Some(native_image_class),
        )
    }
```

Export `BridgeOwner` from `bridge/jni/contract/mod.rs` and `bridge/jni/mod.rs`.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p boltffi_backend --lib bridge::jni::contract::bridge::tests`
Expected: 2 passed.

- [ ] **Step 6: Write the failing host test**

Append to the `tests` module at the bottom of `boltffi_backend/src/target/java/mod.rs` (create the module if absent):

```rust
    #[test]
    fn native_image_binding_requires_native_image_capability() {
        let host = JavaHost::new("com.demo", "Demo").unwrap().binding(JvmBinding::NativeImage);
        let required = host.bridge_capabilities();
        assert!(required.contains(BridgeCapability::NativeImageC));
        assert!(!required.contains(BridgeCapability::Jni));
    }

    #[test]
    fn native_image_class_name_is_reserved() {
        assert!(JavaHost::new("com.demo", "BoltFFINativeImage").is_err());
    }
```

Check `CapabilityRequirements::contains` exists; if not, add `pub fn contains(&self, c: C) -> bool` to it in `capabilities.rs`.

- [ ] **Step 7: Run to verify it fails**

Run: `cargo test -p boltffi_backend --lib target::java::tests`
Expected: compile error, `JvmBinding` not found.

- [ ] **Step 8: Implement `JvmBinding` and host seam**

In `target/java/mod.rs`:

```rust
/// How the Java host reaches the C ABI.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum JvmBinding {
    /// JNI glue compiled as a shared library.
    #[default]
    Jni,
    /// GraalVM Native Image C API, statically linked.
    NativeImage,
}
```

Add `binding: JvmBinding` to `JavaHost` (default in both constructors), `const NATIVE_IMAGE_CLASS: &'static str = "BoltFFINativeImage";`, the builder:

```rust
    /// Selects how generated Java reaches the C ABI.
    pub fn binding(mut self, binding: JvmBinding) -> Self { self.binding = binding; self }
    /// Returns the selected binding.
    pub const fn jvm_binding(&self) -> JvmBinding { self.binding }
```

Extend the name-collision check in `for_version` to also reject `file.as_str().eq_ignore_ascii_case(Self::NATIVE_IMAGE_CLASS)`.

Replace `bridge_capabilities`:

```rust
    fn bridge_capabilities(&self) -> CapabilityRequirements<BridgeCapability> {
        CapabilityRequirements::new().require(match self.binding {
            JvmBinding::Jni => BridgeCapability::Jni,
            JvmBinding::NativeImage => BridgeCapability::NativeImageC,
        })
    }
```

Split `target()` into two: keep `fn target(&self) -> Result<Target<Self, BridgeLayer<CBridge, JniBridge>>>` and add `fn native_image_target(&self) -> Result<Target<Self, BridgeLayer<CBridge, NativeImageBridge>>>` (the bridge type lands in Task 2; for this task make `render_with_coverage` return `Err(Self::unsupported("native image binding"))` for `JvmBinding::NativeImage` so the crate compiles).

In `target/jvm/library.rs` add `DesktopLoader::None` ("no desktop loading; the library is statically linked") with `loads_system() == false`, `loads_bundled() == false`. In `runtime/loader.rs`, when the desktop loader is `None` and Android loading is disabled, `render()` returns `Ok(String::new())` and `desktop_source()` returns `Ok(None)`. Add `NativeLibraries::static_linked() -> Result<Self>` that uses `DesktopLoader::None`, and in `JavaHost::binding(NativeImage)` set `self.native_libraries = NativeLibraries::static_linked()?` (make `binding` return `Result<Self>` if needed; keep the test in Step 6 in sync).

- [ ] **Step 9: Forwarding `Native` members**

In `render/native.rs`, `Method` gets `forward_to: Option<TypeIdentifier>` set from `bridge.native_image_class()` in `from_symbol`/`from_contract` (thread the contract through; `from_contract` currently only takes `NativeMethod` — add a `forward_to` parameter). Template `native_method.java` becomes:

```java
{%- match method.forward_to %}
{%- when Some with (target) %}
    static {{ method.returns }} {{ method.name }}({{ method.parameters }}) {
        {% if !method.returns_void %}return {% endif %}{{ target }}.{{ method.name }}({{ method.argument_names }});
    }
{%- when None %}
    static native {{ method.returns }} {{ method.name }}({{ method.parameters }});
{%- endmatch %}
```

Add `argument_names()` to `Method` returning the comma-joined parameter names. Read the existing template first and keep its exact formatting for the `None` branch so JNI snapshots do not change.

- [ ] **Step 10: Run all backend tests**

Run: `cargo test -p boltffi_backend`
Expected: all pass, no snapshot changes (`cargo insta` not needed). If a Java snapshot changes, the `None` branch formatting drifted; fix the template.

- [ ] **Step 11: Commit**

```bash
git add -A boltffi_backend
git commit -m "feat(java): add JvmBinding seam and NativeImageC capability

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `NativeImageBridge` skeleton with scalar downcalls and the C shim

**Files:**
- Create: `boltffi_backend/src/bridge/native_image/{mod.rs,bridge.rs}`
- Create: `boltffi_backend/src/bridge/native_image/contract/{mod.rs,downcall.rs,shim.rs,java_type.rs}`
- Create: `boltffi_backend/src/bridge/native_image/template/mod.rs`
- Create: `boltffi_backend/templates/bridge/native_image/{native_image.java,support.java,lib.java,wrapper.java,glue.c}`
- Modify: `boltffi_backend/src/bridge/mod.rs`
- Modify: `boltffi_backend/src/target/java/mod.rs` (wire `native_image_target`)
- Test: `boltffi_backend/tests/native_image.rs`, snapshot `boltffi_backend/tests/snapshots/native_image__*.snap`

**Interfaces:**
- Consumes `JniBridgeContract::for_native_image`, `NativeMethod`, `NativeParameterKind`, `NativeReturn`, `c::Function`, `c::Type`.
- Produces:
  ```rust
  pub struct NativeImageBridge { class: JvmClassPath, java_path: FilePath, glue_path: FilePath, library: LibraryName }
  impl NativeImageBridge {
      pub fn new(package: impl Into<String>, java_path: impl Into<PathBuf>, glue_path: impl Into<PathBuf>, library: impl Into<String>) -> Result<Self>;
      pub fn for_host(package: &str, library: &str) -> Result<Self>; // "BoltFFINativeImage.java", "native_image/native_image_glue.c"
  }
  impl BridgeBackend for NativeImageBridge { type Surface = Native; type Input = CBridgeContract; type Contract = JniBridgeContract; }
  ```
- Produces `JavaCType` with `fn from_c(ty: &c::Type) -> Result<JavaCType>` and `fn java(&self) -> &str` mapping: `Void→void`, `Bool→boolean`, `Int8/Uint8→byte`, `Int16/Uint16→short`, `Int32/Uint32→int`, `Int64/Uint64/SignedPointerWidth/PointerWidth→long`, `Float32→float`, `Float64→double`, `Status→int` (via shim), any pointer/`FunctionPointer`/`FutureHandle`→`org.graalvm.word.PointerBase`, `StreamPollResult→byte`, `WaitResult→int`, `CStyleEnum{repr}`→repr mapping. `Buffer`, `String`, `Span`, `CallbackHandle`, `DirectRecord`, `Named` are by-value structs and are only reachable through the shim.
- Produces `Downcall { lib_name: Identifier, c_function: c::Function, needs_shim: bool, transition: Transition, params: Vec<(Identifier, JavaCType)>, returns: JavaCType }` and `ShimFunction { name: Identifier, source: String }`.
- Produces `Transition { NoTransition, ToNative }` decided by: `ToNative` if any parameter group is `Callback`, `Closure`, `Continuation`, `CallbackCompletion`, `ClosureReturn`, `CompletionStatusOut`, or the function's source callable is async, or it is a stream protocol function; else `NoTransition`.

- [ ] **Step 1: Write the failing snapshot test**

`boltffi_backend/tests/native_image.rs`:

```rust
use boltffi_ast::PackageInfo;
use boltffi_backend::{
    bridge::{c::CBridge, jni::JniBridgeContract, native_image::NativeImageBridge},
    core::{BridgeLayer, BridgeOutput, BridgeStack},
};
use boltffi_binding::{Native, lower};

fn bindings(source: &str) -> boltffi_binding::Bindings<Native> {
    let file = syn::parse_str(source).expect("valid source fixture");
    let source = boltffi_scan::scan_file(file, PackageInfo::new("demo", None)).expect("scan");
    lower::<Native>(&source).expect("lower")
}

fn bridge(source: &str) -> BridgeOutput<JniBridgeContract> {
    let stack = BridgeLayer::new(
        CBridge::new("native_image/demo.h").expect("C bridge"),
        NativeImageBridge::for_host("com.boltffi.demo", "demo_core").expect("NI bridge"),
    );
    stack.build(&bindings(source)).expect("bridge stack")
}

fn rendered(source: &str) -> String {
    bridge(source)
        .output()
        .files()
        .iter()
        .map(|f| format!("===== {} =====\n{}", f.path().as_path().display(), f.contents()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn renders_scalar_functions_without_transition() {
    let out = rendered(r#"
        #[export] pub fn add(a: i32, b: i32) -> i32 { a + b }
        #[export] pub fn enabled(flag: bool) -> bool { flag }
        #[export] pub fn refresh() {}
    "#);
    assert!(out.contains("@CFunction(transition = CFunction.Transition.NO_TRANSITION)"));
    assert!(out.contains("static native int boltffi_function_demo_add(int a, int b);"));
    assert!(out.contains("static int boltffi_function_demo_add(int a, int b) {"));
    assert!(out.contains("requireStatic = true"));
    insta::assert_snapshot!(out);
}
```

Confirm the C symbol name `boltffi_function_demo_add` by checking `tests/snapshots/java__java_target_renders_primitive_function_stack.snap`; adjust if the naming differs.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p boltffi_backend --test native_image`
Expected: compile error, `native_image` module missing.

- [ ] **Step 3: Create the module skeleton**

`bridge/native_image/mod.rs`:

```rust
//! GraalVM Native Image bridge layered above the C ABI bridge.
//!
//! Like the JNI bridge, this layer adapts one `CBridgeContract` for the JVM.
//! Unlike JNI, it emits Java, not C: `@CFunction` declarations that call the C
//! ABI directly, Java wrappers with the same signatures the JNI `Native` class
//! exposes, and `@CEntryPoint` trampolines for upcalls. A small C shim covers
//! by-value struct shapes `@CFunction` cannot express.

mod bridge;
mod contract;
mod template;

pub use bridge::NativeImageBridge;
pub use contract::{Downcall, JavaCType, NativeImageContract, ShimFunction, Transition};
```

`bridge/native_image/bridge.rs` implements `BridgeBackend` as in the Interfaces block; `build_contract` calls `JniBridgeContract::for_native_image(self.class.clone(), JvmClassPath::new(package, "BoltFFINativeImage")?, input)`; `render_bridge` builds `NativeImageContract::from_jni(contract, input, &self.library)?` and returns `GeneratedOutput::new(vec![GeneratedFile::new(self.java_path.clone(), java), GeneratedFile::new(self.glue_path.clone(), glue)], Vec::new())`. Implement `sealed::BridgeBackend`.

Add `pub mod native_image;` to `bridge/mod.rs`.

- [ ] **Step 4: Implement `JavaCType` and `Downcall`**

`contract/java_type.rs` per the Interfaces mapping, with `Result` error `Error::UnsupportedTarget { target: "native-image", shape: "by-value struct in @CFunction" }` for the struct variants (callers route those through the shim first).

`contract/downcall.rs`: `Downcall::from_native_method(method: &NativeMethod, stream_symbols: &BTreeSet<Identifier>) -> Result<Self>`:
1. `needs_shim = params.any(is_by_value_struct) || is_by_value_struct(returns)` where by-value struct = `Buffer | String | Span | CallbackHandle(_) | DirectRecord(_) | Named(_) | Status`.
2. If `needs_shim`, `lib_name = Identifier::parse(format!("boltffi_ni_{}", c_function.name()))`, parameters become: each by-value struct param → `ConstPointer(ty)`; a by-value struct return → extra trailing `MutPointer(ty)` out-param named `__out` and Java return `void`; `Status` return → Java `int`.
3. `transition` per the Interfaces rule.

`contract/shim.rs`: `ShimFunction::for_downcall(d: &Downcall) -> Option<Self>` renders:

```c
void boltffi_ni_<name>(<const T *p_i | scalar p_j>..., <R *__out>) {
    *__out = <name>(<*p_i | p_j>...);
}
```
and for `Status` returns `int32_t boltffi_ni_<name>(...) { return <name>(...).code; }`. Use `c::TypeFragment::anonymous(&ty)` for spellings.

- [ ] **Step 5: Templates**

`templates/bridge/native_image/native_image.java`:

```java
package {{ package }};

import java.util.Collections;
import java.util.List;
import org.graalvm.nativeimage.*;
import org.graalvm.nativeimage.c.*;
import org.graalvm.nativeimage.c.function.*;
import org.graalvm.nativeimage.c.struct.*;
import org.graalvm.nativeimage.c.type.*;
import org.graalvm.word.*;

@CContext(BoltFFINativeImage.Directives.class)
@CLibrary(value = "{{ library }}", requireStatic = true)
final class BoltFFINativeImage {
    private BoltFFINativeImage() {}
{% include "bridge/native_image/support.java" %}
{% include "bridge/native_image/lib.java" %}
{%- for wrapper in wrappers %}
{% include "bridge/native_image/wrapper.java" %}
{%- endfor %}
}
```

`support.java` (Task 2 subset; Task 4 extends):

```java
    static final class Directives implements CContext.Directives {
        @Override public List<String> getHeaderFiles() { return Collections.singletonList("<{{ header }}>"); }
        @Override public List<String> getOptions() { return Collections.singletonList("-I" + System.getProperty("boltffi.include", ".")); }
    }
```

`lib.java`:

```java
    static final class Lib {
        private Lib() {}
{%- for d in downcalls %}
        @CFunction({% if d.no_transition %}transition = CFunction.Transition.NO_TRANSITION{% endif %})
        static native {{ d.java_return }} {{ d.lib_name }}({{ d.java_params }});
{%- endfor %}
    }
```

`wrapper.java` for Task 2 handles only `NativeReturn::Void | Value(_)` with `NativeParameterKind::Scalar`:

```java
    static {{ wrapper.java_return }} {{ wrapper.name }}({{ wrapper.java_params }}) {
        {% if !wrapper.returns_void %}return {% endif %}Lib.{{ wrapper.lib_name }}({{ wrapper.arguments }});
    }
```

Any other shape returns `Err(Error::UnsupportedTarget { target: "native-image", shape: "<kind>" })` for now; Tasks 3 and 4 fill them in.

`glue.c`:

```c
#include <stdint.h>
#include <string.h>
#include "{{ header }}"
{% for shim in shims %}
{{ shim.source }}
{%- endfor %}
```

- [ ] **Step 6: Wire the host**

In `target/java/mod.rs` implement `native_image_target()` building `BridgeLayer::new(CBridge::new(self.c_header.clone())?, NativeImageBridge::for_host(&self.package.to_string(), self.native_libraries.desktop_fallback().as_str())?)` and dispatch in `render_with_coverage` on `self.binding`. The header path for Native Image mode defaults to `PathBuf::from("native_image/boltffi.h")` when `binding(NativeImage)` is selected and `c_header` was not customized.

- [ ] **Step 7: Run tests, accept snapshot**

Run: `cargo test -p boltffi_backend --test native_image` then `cargo insta accept` (or `INSTA_UPDATE=always`). Read the snapshot: `Lib` has three `@CFunction` declarations with `NO_TRANSITION`; the wrappers forward; the glue file has zero shims.

- [ ] **Step 8: Add the Java host snapshot**

In `boltffi_backend/tests/java.rs` add a test mirroring `java_target_renders_primitive_function_stack` but with `.binding(JvmBinding::NativeImage)`. Assert `Demo.java` contains `static int boltffi_function_demo_add(int a, int b) {` and `BoltFFINativeImage.boltffi_function_demo_add(a, b)` and no `System.loadLibrary`. Snapshot it as `java__java_native_image_primitive_function_stack`.

- [ ] **Step 9: Run all backend tests, commit**

Run: `cargo test -p boltffi_backend`

```bash
git add -A boltffi_backend
git commit -m "feat(native-image): add NativeImageBridge with scalar downcalls and C shim

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Downcall shapes: records, bytes, status, encoded errors, success-out, vectors, writebacks

**Files:**
- Modify: `boltffi_backend/src/bridge/native_image/contract/downcall.rs`
- Modify: `boltffi_backend/templates/bridge/native_image/{support.java,wrapper.java}`
- Modify: `boltffi_backend/src/bridge/jni/contract/record/parameter.rs` (add `Transport`)
- Modify: `boltffi_backend/src/target/java/render/record.rs` and the record call sites (read `Transport`)
- Test: `boltffi_backend/tests/native_image.rs`

**Interfaces:**
- Produces `pub enum Transport { DirectBuffer, ByteArray }` with `RecordParameter::transport(&self) -> Transport`. `for_native_image` sets `ByteArray`; `from_c_bridge` keeps `DirectBuffer`.
- Java wrapper signature rules (must equal JNI `Native` signatures except record params, which are `byte[]` under `ByteArray`):
  - `Scalar` → primitive.
  - `Bytes` → `java.nio.ByteBuffer` + `int` length exactly as JNI (check `BytesParameter` for the JNI spelling and copy it).
  - `DirectVector` → primitive array (`int[]`, `double[]`, …).
  - `Record` → `byte[]` (ByteArray transport).
  - `Callback`/`Closure`/`Continuation` → `long` (bodies in Task 4).
- Return rules: `Bytes`→`byte[]`, `Record`→`byte[]`, `Status`→`void` (throws), `StatusWriteback`→`byte[]`, `StatusValue`/`EncodedErrorValue`→per `SuccessOutReturn`, `EncodedError`→`void`, `Callback`→`long`.
- Support helpers in `support.java` (all `static`, package-private):
  - `byte[] takeBuffer(FfiBuf pointer)` copies `len` bytes then calls `Lib.<free_buffer>(pointer)`.
  - `void throwStatus(int code)` and `void throwErrorBuffer(FfiBuf pointer)` raising the same exception types the JNI glue raises (read `templates/bridge/jni/runtime/exception.c` for class names and messages and mirror them).
  - `PinnedObject` use for `byte[]` and primitive arrays: `try (PinnedObject pin = PinnedObject.create(array)) { ... pin.addressOfArrayElement(0) ... }`.
  - `@CStruct("FfiBuf_u8") interface FfiBuf extends PointerBase { @CField("ptr") CCharPointer ptr(); @CField("len") long len(); }` and matching `FfiStatus`-free `int` codes.
  - Direct records on the stack: `StackValue.get(size)` where `size` is the C record's `sizeof` — expose `c::Record::size()` if present, else emit `SizeOf.get(<@CStruct interface>.class)` with one generated `@CStruct("<Name>") interface <Name>Struct extends PointerBase {}` per direct record.

- [ ] **Step 1: Write failing tests** for each shape in `tests/native_image.rs`: `echo_point(Point)->Point` (direct record), `greet(name: String)->String` (encoded bytes), `fallible() -> Result<(), MyError>` (status), `fallible_value() -> Result<i32, MyError>` (success-out), `sum(values: &[i32]) -> i64` (direct vector), and a `&mut` record parameter (writeback). Assert on the wrapper signature strings and snapshot the whole output. Use fixtures from `boltffi_backend/tests/source/` where an equivalent exists.

- [ ] **Step 2: Run, verify each fails** with `UnsupportedTarget`.

- [ ] **Step 3: Implement shapes** in `downcall.rs` (`WrapperBody` enum with one variant per shape and an Askama `match` in `wrapper.java`) following `templates/bridge/jni/method/{records.c,call.c,writebacks.c,error_return.c}` line by line, translating each C step into Java:
  - record param: `try (PinnedObject p = PinnedObject.create(bytes)) { Lib.boltffi_ni_x(p.addressOfArrayElement(0), ...) }`
  - record return: `byte[] out = new byte[SIZE]; try (PinnedObject p = ...) { Lib.boltffi_ni_x(..., p.addressOfArrayElement(0)); } return out;`
  - status: `int code = Lib.x(...); if (code != 0) throwStatus(code);`
  - encoded error: `FfiBuf err = StackValue.get(FfiBuf.class); Lib.boltffi_ni_x(..., err); if (err.ptr().isNonNull() || err.len() != 0) throwErrorBuffer(err);`

- [ ] **Step 4: `Transport`** on `RecordParameter`, set in `for_native_image`, read by `render/record.rs` to choose `toByteArray()` vs `toDirectBuffer()`. Add a Java host snapshot for the direct record stack in Native Image mode.

- [ ] **Step 5: Run all backend tests, accept new snapshots, commit**

```bash
git commit -am "feat(native-image): render record, bytes, status and error downcalls

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: Upcalls: handle cells, attach prologue, callbacks, closures, streams, completions

**Files:**
- Create: `boltffi_backend/src/bridge/native_image/contract/upcall.rs`
- Create: `boltffi_backend/templates/bridge/native_image/trampoline.java`
- Modify: `boltffi_backend/templates/bridge/native_image/support.java`
- Modify: `boltffi_backend/src/bridge/native_image/contract/downcall.rs` (Callback/Closure/Continuation param bodies)
- Test: `boltffi_backend/tests/native_image.rs`

**Interfaces:**
- Consumes `CallbackRegistration`, `CallbackMethod`, `ClosureRegistration`, `StreamProtocolMethods`, `CallbackCompletionInvoker`, `CallbackHandleLifecycle` from the JNI contract; `c::Callback::vtable()`, `c::CallbackSlot`.
- Produces `Trampoline { name: Identifier, entry_symbol: Identifier, params: Vec<(Identifier, JavaCType)>, returns: JavaCType, dispatch: DispatchCall }` where `DispatchCall` names the host-rendered static Java dispatcher and its argument conversions (read `templates/bridge/jni/callback/method/invoke/*.c` and `bridge/jni/contract/callback/*.rs` for the exact dispatcher class/method names the JNI glue calls with `CallStaticVoidMethod`; reuse the same names).
- `support.java` additions, verbatim from the spike:

```java
    @CStruct("BoltFFINativeImageHandleCell") interface HandleCell extends PointerBase {
        @CField("isolate") Isolate isolate(); @CField("isolate") void isolate(Isolate v);
        @CField("token") long token();       @CField("token") void token(long v);
    }
    static final class AttachPrologue implements com.oracle.svm.guest.staging.c.function.CEntryPointOptions.Prologue {
        @com.oracle.svm.shared.Uninterruptible(reason = "prologue")
        static int enter(HandleCell cell) {
            return com.oracle.svm.guest.staging.c.function.CEntryPointActions.enterAttachThread(cell.isolate(), true);
        }
    }
    static long newHandleCell(long token) {
        HandleCell cell = UnmanagedMemory.malloc(SizeOf.get(HandleCell.class));
        cell.isolate(CurrentIsolate.getIsolate());
        cell.token(token);
        return cell.rawValue();
    }
```
  and the shim header gains `typedef struct { void *isolate; int64_t token; } BoltFFINativeImageHandleCell;` (emit it into `glue.c` and a tiny generated `native_image_glue.h` the Directives also include).
- Each vtable gets one unmanaged instance allocated lazily in a static initializer: `static final <Name>VTable VT_<Name> = ...; VT.free(FREE_<Name>.getFunctionPointer()); ...` using `CEntryPointLiteral.create(BoltFFINativeImage.class, "<trampoline>", HandleCell.class, ...)`.
- Callback parameter (`long token` from Java) → `Lib.<create_handle>` is not called; instead the wrapper passes `(newHandleCell(token), VT_<Name>)` as the two words of `BoltFFICallbackHandle` through the shim (`const BoltFFICallbackHandle *`) built on the stack: `StackValue.get(CallbackHandleStruct.class)` with `handle`/`vtable` fields.
- `free` trampoline: calls the host's free dispatcher with `cell.token()`, then `UnmanagedMemory.free(cell)`.
- `clone` trampoline: calls host's clone dispatcher, returns `newHandleCell(newToken)`.

- [ ] **Step 1: Write failing tests** for: a sync callback trait with a scalar method, a callback with an encoded (`String`) argument, a closure parameter, an async function with a continuation, and a stream subscription. Assert the presence of `@CEntryPoint(name = "...")`, `@CEntryPointOptions(prologue = AttachPrologue.class)`, `CEntryPointLiteral.create(`, and snapshot.

- [ ] **Step 2: Run, verify failures.**

- [ ] **Step 3: Implement `upcall.rs` and `trampoline.java`:**

```java
    @CEntryPoint(name = "{{ t.entry_symbol }}")
    @com.oracle.svm.guest.staging.c.function.CEntryPointOptions(prologue = AttachPrologue.class)
    static {{ t.java_return }} {{ t.name }}(HandleCell cell{% for p in t.params %}, {{ p.java }} {{ p.name }}{% endfor %}) {
{{ t.body }}
    }
```
  Bodies mirror `templates/bridge/jni/callback/method/*.c`: convert byte pointers + lengths to `byte[]` (`CTypeConversion.asByteBuffer(ptr, len)` then `get`), records via `byte[]` copy, nested callback handles via `newHandleCell`, then call the host dispatcher with `cell.token()`. Failure paths call the same completion-failure functions the JNI templates call (`callback/method/fail.c`).

- [ ] **Step 4: Continuations and completions.** Continuation parameter (`long callback_data`) pairs with one generated `@CEntryPoint` per continuation signature (`RustFutureContinuationCallback`, `StreamContinuationCallback`) whose first param is `HandleCell` holding the Java future token. Completion invokers call `Lib` functions directly (they are downcalls).

- [ ] **Step 5: Run all backend tests, accept snapshots, commit**

```bash
git commit -am "feat(native-image): render callback, closure, stream and async trampolines

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: Packaging and config

**Files:**
- Modify: `boltffi_cli/src/config/targets/*.rs` (the `JavaJvmConfig` struct), `boltffi_cli/src/config/mod.rs`
- Create: `boltffi_cli/src/pack/java/native_image.rs`
- Modify: `boltffi_cli/src/pack/java/{mod.rs,plan.rs}`
- Modify: `BOLTFFI_TOML_SPEC.md:271-300`
- Test: `boltffi_cli` unit tests beside the config parser and a `pack` plan test

**Interfaces:**
- Config: `[targets.java.jvm] binding = "jni" | "native-image"` (default `jni`), `Config::java_jvm_binding() -> JavaJvmBinding`.
- `pack java` with `native-image`:
  1. `cargo build` with `--crate-type staticlib` requirement (error if the crate does not build a staticlib; reuse `SelectedLibrary::builds_staticlib`).
  2. Compile `native_image/native_image_glue.c` with the same compiler discovery `compile_jni_library` uses (`link.rs:399`), into `native/libboltffi_<crate>_glue.a` (`ar rcs`, or `lib.exe` on MSVC).
  3. Copy `lib<crate>.a`, header, glue header into `dist/java/native/`.
  4. Write `dist/java/META-INF/native-image/<package>/<artifact>/native-image.properties`:
     ```
     Args = -H:CLibraryPath=${.}/../../../../native \
            -Dboltffi.include=${.}/../../../../native \
            --add-exports org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c.function=ALL-UNNAMED \
            --add-exports org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c=ALL-UNNAMED \
            --add-exports org.graalvm.nativeimage.shared/com.oracle.svm.shared=ALL-UNNAMED
     ```
  5. Skip JNI shared-library build, strip, and debug-symbol steps.

- [ ] **Step 1: Failing config test** parsing `binding = "native-image"`; verify `java_jvm_binding()`.
- [ ] **Step 2: Implement config**, thread to `JavaHost::binding` in the generate path (`boltffi_cli/src/target.rs` / wherever `JavaHost::new` is called; grep `JavaHost::`).
- [ ] **Step 3: Failing pack plan test** asserting the output file list for native-image mode (properties path, `.a` names, no `_jni` dylib).
- [ ] **Step 4: Implement `native_image.rs`** and dispatch in `plan.rs`.
- [ ] **Step 5: Update `BOLTFFI_TOML_SPEC.md`**, run `cargo test -p boltffi_cli`, commit:

```bash
git commit -am "feat(cli): pack java native-image mode with static archives and properties

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Gated end-to-end Native Image runtime test

**Files:**
- Create: `boltffi_backend/tests/native_image_runtime.rs`
- Reuse: `boltffi_backend/tests/java_toolchain/mod.rs` patterns from `java_runtime.rs`

**Interfaces:**
- Toolchain discovery: `native-image` from `$GRAALVM_HOME/bin`, then `$JAVA_HOME/bin`, then `PATH`. If absent, the test prints `skipping: native-image not found` and returns `Ok(())`.
- `javac` classpath includes `<graal>/lib/svm/builder/svm-guest-staging.jar` and `<graal>/lib/svm/builder/svm-shared.jar`.
- One fixture crate (Rust source string, like `RUST_SOURCE` in `java_runtime.rs`) covering: scalar round trips, direct record, encoded record, `Result` error, a sync callback, a callback invoked from a Rust `std::thread::spawn` (foreign-thread attach), an async function, and a stream with three items.
- One `Main.java` that exercises each and prints `OK <case>` lines; the test asserts all lines and exit code 0.

- [ ] **Step 1: Write the test** using `JavaHost::new(pkg, "Demo")?.binding(JvmBinding::NativeImage)` to generate, `cargo build --release` the fixture as `staticlib`, `cc -c` the glue, `ar rcs`, `javac`, then `native-image` with the five flags from Task 5 and `-H:CLibraryPath=<native>` `-Dboltffi.include=<native>`.
- [ ] **Step 2: Run** `cargo test -p boltffi_backend --test native_image_runtime -- --nocapture`. Expected: passes locally (GraalVM 25 at `/Library/Java/JavaVirtualMachines/gvm.jdk25/Contents/Home`); skips when `native-image` is missing.
- [ ] **Step 3: Fix any runtime defects found in Tasks 3 and 4** (each fix gets its own snapshot assertion).
- [ ] **Step 4: Commit**

```bash
git commit -am "test(native-image): add gated end-to-end image test with foreign-thread upcalls

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: Documentation

**Files:**
- Create: `docs/src/content/docs/native-image.mdx`
- Modify: `docs/src/content/docs/packaging.mdx` (link), `README.md` (one bullet under targets if such a list exists)

- [ ] **Step 1: Write the page**: what it is, config snippet, required GraalVM version and the `--add-exports` note, how `native-image.properties` is picked up from the classpath, the `boltffi.include` property, and the static-link requirement. Include a full minimal `native-image` command line.
- [ ] **Step 2: Build docs** (`cd docs && bun run build` or `npm run build`), fix broken links.
- [ ] **Step 3: Commit**

```bash
git commit -am "docs: add Native Image backend guide

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Subagent Assignments (max 4 concurrent)

- Tasks 1, 2, 4 are the integration core: done inline by the lead.
- Task 3 shape work can be split: one Sonnet agent for record/vector/writeback wrappers, one for bytes/status/error/success-out, each with its own tests, after the `WrapperBody` enum from Task 2 lands.
- Task 5 (Sonnet), Task 6 (Sonnet), Task 7 (Haiku) can run in parallel once Task 4 is merged.
- Snapshot fixture authoring for Tasks 3 and 4 is a Haiku job given the shape list above.

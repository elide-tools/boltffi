# Native Image C API backend for Java

Date: 2026-09-14
Status: approved design, implementation in progress on `feat/native-image-c-api`

## Goal

Let a GraalVM Native Image application (Elide) call BoltFFI-generated Rust
libraries through the Native Image C API (`org.graalvm.nativeimage.c`)
instead of JNI. The Rust core, the C ABI bridge, and the public Java API are
unchanged. Only the layer between the Java `Native` owner class and the C ABI
changes.

Decisions already made:

- Native Image is the only runtime this backend targets. Generated code does
  not run on HotSpot. No runtime switch between JNI and C API.
- The Rust library is statically linked into the image. No `loadLibrary`.

## Non-goals

- A pointer-native Java API (`PointerBase`, `StackValue` records) exposed to
  users. Transport stays contract-driven so this can come later.
- Kotlin or KMP support on the new bridge. The bridge is JVM-generic by
  construction, but only the Java host is wired in this work.
- Panama FFM.

## Architecture

The backend already composes `Target<Host, BridgeLayer<CBridge, JniBridge>>`.
The C bridge is the base layer and owns symbols, direct-record layouts,
callback vtables, stream protocols, async continuations, and buffer release.
The JNI bridge layers on top and adapts that contract for the JVM.

This work adds a sibling upper layer:

```
Target<JavaHost, BridgeLayer<CBridge, NativeImageBridge>>
```

`NativeImageBridge` implements `BridgeBackend` with `Input = CBridgeContract`
and `Contract = JniBridgeContract`. It reuses `JniBridgeContract::from_c_bridge`
for every JVM-facing fact (Java parameter kinds, return shapes, callback
methods, closure registrations, stream batch methods, success-out writers).
Those facts describe how Java talks to the owner class, not how the owner
class talks to C, so they are the same in both modes. Where a fact genuinely
differs, the contract carries it as a value (see Transport below), never as a
second type.

The bridge renders one Java file, `Native.java`, instead of `jni_glue.c`.

### Capabilities

Add `BridgeCapability::NativeImageC`. The Native Image bridge advertises
`CAbi` plus `NativeImageC`. `JavaHost` gains a `JvmBinding { Jni, NativeImage }`
selector and requires `Jni` or `NativeImageC` accordingly. `Target` rejects
mismatched pairings at build time as it does today.

### Generated `Native.java`

Package-private `final class Native` in the host package, containing:

1. `@CContext(Native.Directives.class)` with a `Directives` class returning
   the generated header (`"<boltffi.h>"` via search path) and
   `@CLibrary(value = <lib>, requireStatic = true)`.
2. A nested `Lib` holder of `@CFunction` declarations, one per C bridge
   export, with C-typed parameters (`long`, `int`, `PointerBase` subclasses,
   `CCharPointer`). Functions whose parameter groups contain no callback,
   closure, continuation, or completion-status shape and whose execution is
   synchronous are marked `transition = Transition.NO_TRANSITION`.
3. Static wrapper methods with exactly the names and Java signatures the JNI
   `Native` class has today (`static byte[] boltffi_...(java.nio.ByteBuffer)`,
   `static long ...`, etc.), implemented in Java: pin arrays, read direct
   buffer addresses, call `Lib`, copy owned Rust buffers into `byte[]`, call
   the C release function, translate status and encoded error buffers into the
   same exceptions the JNI glue throws.
4. `@CEntryPoint` upcall trampolines for every callback vtable slot, closure
   trampoline, stream batch pop, and async completion. Each trampoline's first
   parameter is the `uint64_t handle` the C vtable already passes.
5. Unmanaged `HandleCell` struct (`@CStruct`): `{ Isolate isolate; long token; }`.
   Registering a Java callback allocates a cell with `UnmanagedMemory.malloc`,
   stores the current isolate and the Java registry token, and hands the cell
   address to the C `create_handle` function as the handle. `free` slots free
   the cell.
6. A custom `@CEntryPointOptions(prologue = AttachPrologue.class)` whose
   prologue reads `isolate` from the `HandleCell` at parameter 0 without
   touching the Java heap and calls `CEntryPointActions.enterAttachThread`.
   Foreign Rust threads attach on first upcall and stay attached. Epilogue is
   the default leave.

Host-rendered callback dispatchers (`Native`-adjacent static methods that map
a token to a Java object and invoke it) are called directly from the
trampolines as plain Java calls. Global JNI references become `ObjectHandles`
where the JNI glue held them.

### Transport

`RecordParameter` and related contract values gain a `Transport` enum:
`DirectBuffer` (JNI default) or `ByteArray`. The Native Image bridge selects
`ByteArray` for direct records so the wrapper can pin and pass the address.
The host renderer reads the enum when choosing `toDirectBuffer()` versus
`toByteArray()`. Public record API is unchanged. A later `StackValue` variant
slots in here.

### Host changes

- `JavaHost::binding(JvmBinding)` builder. Default stays `Jni`.
- `Module` render skips its own `Native` class and `BoltFFINativeRuntime`
  loader when the bridge contract reports it owns the native class
  (`JniBridgeContract::owner_rendered_by_bridge()`).
- `bridge_capabilities()` follows the selected binding.
- Record transport reads the contract enum.

### Packaging

`pack java` gains a `native-image` output mode:

- Builds the Rust crate as `staticlib`, stages `libboltffi_<crate>.a` and the
  generated header under `native/`.
- Emits `META-INF/native-image/<group>/<artifact>/native-image.properties`
  with `Args = -H:CLibraryPath=${.}/../../../../native`.
- Emits no JNI glue and no `jni-config.json`.

Config: `[targets.java.jvm] binding = "jni" | "native-image"`.

## Error handling

- Status returns and encoded error buffers map to the same Java exception
  types the JNI path throws, so host-rendered `result.rs` code is unchanged.
- A trampoline that cannot attach (`enterAttachThread` non-zero) invokes the
  same failure completion the JNI glue uses, then returns the failure value.
- Unsupported shapes fail at generation time with `Error::UnsupportedTarget`,
  never with a runtime stub.

## Testing

- Snapshot tests for `Native.java` covering: primitive functions, direct
  records, encoded records, classes/handles, callbacks, closures, streams,
  async completions, constants, errors.
- One aggregated `native_image_runtime.rs` integration test that generates a
  fixture, builds a Rust staticlib, compiles Java, runs `native-image` once,
  and executes the binary. Gated on `native-image` being discoverable
  (`GRAALVM_HOME`, `JAVA_HOME`, or PATH); skipped otherwise. Includes a
  foreign-thread upcall case.
- Existing JNI snapshot and runtime tests must stay green.

## Spike

Before the bridge is written, a throwaway image under the session scratchpad
proves two things with the local GraalVM 25 toolchain:

1. `@CFunction` into a Rust `staticlib` with `@CLibrary(requireStatic = true)`.
2. A `@CEntryPoint` invoked from a Rust `std::thread` via a function pointer
   whose first argument is a `HandleCell*`, using the attach prologue.

The spike's output is a recommendation on prologue mechanics, not code that
is kept.

## Work order

1. Spike (above).
2. `BridgeCapability::NativeImageC`, `JvmBinding`, host seam, contract
   `Transport` and `owner_rendered_by_bridge`.
3. `NativeImageBridge` skeleton: directives, `Lib` declarations, sync scalar
   and record wrappers, snapshot tests.
4. Upcalls: `HandleCell`, prologue, callback vtables, closures, then streams
   and async completions.
5. Encoded bytes, owned buffers, status and error translation.
6. Packaging and config.
7. Integration test, docs.

Steps 3 through 7 can farm out snapshot fixtures, test generation, and docs
to subagents once the contract shapes are fixed.

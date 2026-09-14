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

The bridge renders two files instead of `jni_glue.c`: `Native.java` and a
thin pointer-ABI C shim, `native_image_glue.c`.

### Why a C shim is still needed (spike finding)

`@CFunction` cannot pass or return structs by value. The C ABI bridge does so
pervasively: owned buffers (`FfiBuf_u8`, 32 bytes) and direct records return
by value, direct records and `BoltFFICallbackHandle` (16 bytes) pass by value,
and `FfiStatus` is a one-field struct. Two-word structs happen to travel in
two registers on arm64 and x86_64 SysV, which the spike confirmed, but
anything larger uses `sret` or stack memory that Java cannot express, and
Windows x64 differs again.

The shim therefore wraps every C export whose signature carries a by-value
struct in a `boltffi_ni_*` function that takes and returns pointers only:
`void boltffi_ni_echo_point(const Point *in, Point *out)`,
`void boltffi_ni_fetch(FfiBuf_u8 *out, ...)`, `int32_t` for `FfiStatus`.
Exports that are already scalar or pointer only are called directly. Callback
vtables are filled by Java, so `create_handle` is not called from Java.

The shim is mechanical, has no JNI or JVM dependency, and is compiled into the
same static library by `pack java`, which already compiles `jni_glue.c`.

### Capabilities

Add `BridgeCapability::NativeImageC`. The Native Image bridge advertises
`CAbi` plus `NativeImageC`. `JavaHost` gains a `JvmBinding { Jni, NativeImage }`
selector and requires `Jni` or `NativeImageC` accordingly. `Target` rejects
mismatched pairings at build time as it does today.

### Generated `Native.java`

Package-private `final class Native` in the host package, containing:

1. `@CContext(Native.Directives.class)` with a `Directives` class returning
   the generated header (`"<boltffi.h>"`) plus `getOptions()` returning
   `-I<dir>` read from the build-time system property `boltffi.include`, and
   `@CLibrary(value = <lib>, requireStatic = true)`. `-H:CLibraryPath` is for
   libraries only; header include paths must come from the directives.
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
   prologue `static int enter(HandleCell cell)` reads `isolate` from the cell
   at parameter 0 without touching the Java heap and returns
   `CEntryPointActions.enterAttachThread(isolate, true)`. This mirrors the
   prologue Native Image's own JNI `AttachCurrentThread` uses. Foreign Rust
   threads attach on first upcall and stay attached. Epilogue is the default
   leave. Verified by the spike on GraalVM 25 with a Rust `std::thread`.

   These live in internal modules in GraalVM 25: `CEntryPointOptions`,
   `CEntryPointActions` in `com.oracle.svm.guest.staging.c.function`
   (`org.graalvm.nativeimage.guest.staging`) and `Uninterruptible` in
   `com.oracle.svm.shared` (`org.graalvm.nativeimage.shared`). The generated
   `native-image.properties` carries the three `--add-exports` flags, and the
   `javac` classpath needs `lib/svm/builder/svm-guest-staging.jar` and
   `svm-shared.jar`. Elide already builds against these internals. The
   `staging` name signals Oracle intends to promote them; the bridge isolates
   every use behind one generated `NativeImageSupport` class so a package
   rename is a one-line template change.

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

- Builds the Rust crate as `staticlib`, compiles `native_image_glue.c` into a
  second static archive, and stages both plus the generated header under
  `native/`.
- Emits `META-INF/native-image/<group>/<artifact>/native-image.properties`
  with `Args = -H:CLibraryPath=${.}/../../../../native
  -Dboltffi.include=${.}/../../../../native` plus the three `--add-exports`
  flags above.
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

## Spike (done 2026-09-14)

A throwaway image under the session scratchpad, built with the local GraalVM
25.3 toolchain, proved:

1. `@CFunction(transition = NO_TRANSITION)` into a Rust `staticlib` linked via
   `@CLibrary(requireStatic = true)` and `-H:CLibraryPath`.
2. A `@CEntryPoint` invoked synchronously and then three times from a Rust
   `std::thread`, through a vtable whose first argument is a `HandleCell*`,
   using the attach prologue. The foreign thread appeared as `System-0`,
   `free` ran on it, and the process exited cleanly.
3. A 16-byte by-value struct parameter is ABI-equivalent to two scalar
   parameters on arm64. Larger by-value structs are not expressible, hence
   the shim.

Nothing from the spike is kept.

## Work order

1. Spike (above).
2. `BridgeCapability::NativeImageC`, `JvmBinding`, host seam, contract
   `Transport` and `owner_rendered_by_bridge`.
3. `NativeImageBridge` skeleton: directives, `Lib` declarations, C shim for
   by-value shapes, sync scalar and record wrappers, snapshot tests.
4. Upcalls: `HandleCell`, prologue, callback vtables, closures, then streams
   and async completions.
5. Encoded bytes, owned buffers, status and error translation.
6. Packaging and config.
7. Integration test, docs.

Steps 3 through 7 can farm out snapshot fixtures, test generation, and docs
to subagents once the contract shapes are fixed.

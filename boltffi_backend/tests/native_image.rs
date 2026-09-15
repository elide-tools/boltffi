use boltffi_ast::PackageInfo;
use boltffi_backend::{
    bridge::{c::CBridge, jni::JniBridgeContract, native_image::NativeImageBridge},
    core::{BridgeLayer, BridgeOutput, BridgeStack},
};
use boltffi_binding::{Native, lower};

const PRIMITIVE_FUNCTIONS: &str = r#"
    #[export]
    pub fn add(a: i32, b: i32) -> i32 { a + b }

    #[export]
    pub fn enabled(flag: bool) -> bool { flag }

    #[export]
    pub fn refresh() {}
"#;

fn bindings(source: &str) -> boltffi_binding::Bindings<Native> {
    let file = syn::parse_str(source).expect("valid source fixture");
    let source =
        boltffi_scan::scan_file(file, PackageInfo::new("demo", None)).expect("fixture should scan");
    lower::<Native>(&source).expect("fixture should lower")
}

fn bridge(source: &str) -> BridgeOutput<JniBridgeContract> {
    let stack = BridgeLayer::new(
        CBridge::new("native_image/demo.h").expect("C header bridge"),
        NativeImageBridge::for_host("com.boltffi.demo", "demo_core").expect("Native Image bridge"),
    );
    stack
        .build(&bindings(source))
        .expect("Native Image bridge stack")
}

fn rendered(source: &str) -> String {
    bridge(source)
        .output()
        .files()
        .iter()
        .filter(|file| {
            file.path()
                .as_path()
                .extension()
                .is_some_and(|ext| ext != "h")
        })
        .map(|file| {
            format!(
                "===== {} =====\n{}",
                file.path().as_path().display(),
                file.contents()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn renders_scalar_functions_without_transition() {
    let out = rendered(PRIMITIVE_FUNCTIONS);
    assert!(out.contains("===== com/boltffi/demo/BoltFFINativeImage.java ====="));
    assert!(out.contains("===== native_image/native_image_glue.c ====="));
    assert!(out.contains("@CFunction(transition = CFunction.Transition.NO_TRANSITION)"));
    assert!(out.contains("static native int boltffi_function_demo_add(int a, int b);"));
    assert!(out.contains("static int boltffi_function_demo_add(int a, int b) {"));
    assert!(out.contains("return Lib.boltffi_function_demo_add(a, b);"));
    assert!(out.contains("static native void boltffi_function_demo_refresh();"));
    assert!(out.contains("@CLibrary(value = \"demo_core\", requireStatic = true)"));
    assert!(out.contains("java.util.Arrays.asList(\"<demo.h>\", \"<native_image_glue.h>\")"));
    insta::assert_snapshot!(out);
}

#[test]
fn contract_advertises_native_image_capability_and_owner() {
    use boltffi_backend::{
        bridge::jni::BridgeOwner,
        core::{BridgeCapability, BridgeContract},
    };
    let output = bridge(PRIMITIVE_FUNCTIONS);
    let contract = output.contract();
    assert_eq!(contract.owner(), BridgeOwner::NativeImage);
    assert!(
        contract
            .capabilities()
            .status(BridgeCapability::NativeImageC)
            .is_stable()
    );
    assert!(
        !contract
            .capabilities()
            .status(BridgeCapability::Jni)
            .is_stable()
    );
}

const DIRECT_RECORD: &str = r#"
    #[repr(C)]
    #[data]
    pub struct Point {
        pub x: f64,
        pub y: f64,
    }

    #[export]
    pub fn echo_point(point: Point) -> Point { point }

    #[export]
    pub fn nudge(point: &mut Point, by: f64) { point.x += by; }
"#;

const ENCODED_BYTES: &str = r#"
    #[data]
    pub struct Named {
        pub name: String,
        pub tags: Vec<String>,
    }

    #[export]
    pub fn greet(name: String) -> String { format!("hi {name}") }

    #[export]
    pub fn describe(value: Named) -> Named { value }

    #[data(impl)]
    impl Named {
        pub fn rename(&mut self, name: String) { self.name = name; }
    }
"#;

const ERRORS: &str = r#"
    #[error]
    pub struct AppError {
        pub code: i32,
        pub message: String,
    }

    #[error]
    pub enum ParseError {
        Missing,
        Invalid,
    }

    #[export]
    pub fn may_fail(valid: bool) -> Result<String, AppError> {
        match valid {
            true => Ok("ok".to_owned()),
            false => Err(AppError { code: 400, message: "invalid".to_owned() }),
        }
    }

    #[export]
    pub fn parse(valid: bool) -> Result<i32, ParseError> {
        if valid { Ok(1) } else { Err(ParseError::Invalid) }
    }

    #[export]
    pub fn check(valid: bool) -> Result<(), ParseError> {
        if valid { Ok(()) } else { Err(ParseError::Missing) }
    }
"#;

const DIRECT_VECTORS: &str = r#"
    #[export]
    pub fn sum(values: &[i32]) -> i64 { values.iter().map(|v| *v as i64).sum() }

    #[export]
    pub fn increment(values: &mut [u64]) {
        values.iter_mut().for_each(|value| *value += 1);
    }
"#;

#[test]
fn renders_direct_record_downcalls_through_the_shim() {
    let out = rendered(DIRECT_RECORD);
    // by-value record in and out goes through a pointer shim
    assert!(out.contains("static native void boltffi_ni_boltffi_function_demo_echo_point(org.graalvm.word.PointerBase point, org.graalvm.word.PointerBase __out);"));
    assert!(
        out.contains("static byte[] boltffi_function_demo_echo_point(java.nio.ByteBuffer point) {")
    );
    assert!(out.contains("directAddress(point, SizeOf.get(___PointStruct.class))"));
    assert!(out.contains("byte[] __result_record = new byte[SizeOf.get(___PointStruct.class)];"));
    assert!(
        out.contains("@CStruct(\"___Point\")\n    interface ___PointStruct extends PointerBase {}")
    );
    // the C shim writes the by-value return through the out pointer
    assert!(out.contains("void boltffi_ni_boltffi_function_demo_echo_point(const ___Point *point, ___Point *__out) {\n    *__out = boltffi_function_demo_echo_point(*point);\n}"));
    // `&mut Point` is a pointer receive with a status return
    assert!(out.contains(
        "checkStatus(Lib.boltffi_ni_boltffi_function_demo_nudge(__boltffi_point_ptr, by));"
    ));
    insta::assert_snapshot!(out);
}

#[test]
fn renders_encoded_bytes_and_owned_buffer_returns() {
    let out = rendered(ENCODED_BYTES);
    assert!(out.contains("static byte[] boltffi_function_demo_greet(java.nio.ByteBuffer name, int __boltffi_name_len) {"));
    assert!(out.contains("directAddress(name, __boltffi_name_len)"));
    assert!(out.contains("FfiBuf __result_buffer = StackValue.get(FfiBuf.class);"));
    assert!(out.contains("return takeBuffer(__result_buffer);"));
    assert!(out.contains(
        "static native void boltffi_ni_boltffi_free_buf(org.graalvm.word.PointerBase buf);"
    ));
    assert!(out.contains(
        "void boltffi_ni_boltffi_free_buf(const FfiBuf_u8 *buf) {\n    boltffi_free_buf(*buf);\n}"
    ));
    insta::assert_snapshot!(out);
}

#[test]
fn renders_status_and_encoded_error_returns() {
    let out = rendered(ERRORS);
    assert!(out.contains("checkErrorBuffer(__error);"));
    assert!(out.contains("static native void boltffi_ni_boltffi_function_demo_parse(boolean valid, org.graalvm.word.PointerBase return_out, org.graalvm.word.PointerBase __out);"));
    assert!(out.contains("StackValue.get(org.graalvm.nativeimage.c.type.CIntPointer.class);"));
    assert!(out.contains("static void boltffi_function_demo_check(boolean valid) {"));
    assert!(out.contains("static int boltffi_function_demo_parse(boolean valid) {"));
    assert!(out.contains("static byte[] boltffi_function_demo_may_fail(boolean valid) {"));
    insta::assert_snapshot!(out);
}

#[test]
fn renders_direct_vectors_through_pinned_arrays() {
    let out = rendered(DIRECT_VECTORS);
    assert!(out.contains("static long boltffi_function_demo_sum(int[] values) {"));
    assert!(out.contains("try (PinnedObject __pin_values = PinnedObject.create(values)) {"));
    assert!(out.contains("__pin_values.addressOfArrayElement(0), (long) values.length"));
    assert!(out.contains("static void boltffi_function_demo_increment(long[] values) {"));
    insta::assert_snapshot!(out);
}

/// Debug helper: `BOLTFFI_NI_FIXTURE=errors cargo test --test native_image dump_fixture -- --ignored --nocapture`.
#[test]
#[ignore]
fn dump_fixture() {
    let source = match std::env::var("BOLTFFI_NI_FIXTURE").as_deref() {
        Ok("record") => DIRECT_RECORD,
        Ok("bytes") => ENCODED_BYTES,
        Ok("errors") => ERRORS,
        Ok("vectors") => DIRECT_VECTORS,
        Ok("callbacks") => CALLBACKS,
        _ => PRIMITIVE_FUNCTIONS,
    };
    eprintln!("{}", rendered(source));
}

const CALLBACKS: &str = r#"
    #[export]
    pub trait Listener {
        fn on_value(&self, value: i32) -> i32;
        fn on_text(&self, text: &str);
        fn describe(&self) -> String;
        fn on_batch(&self, values: &[i64]) -> bool;
    }

    #[export]
    pub fn subscribe(listener: Box<dyn Listener>) -> i32 { listener.on_value(1) }
"#;

#[test]
fn renders_callback_trampolines_and_vtable_registration() {
    let out = rendered(CALLBACKS);
    // Java side: entry points with the attach prologue, one per slot plus free and clone
    assert!(out.contains("CEntryPointOptions(prologue = AttachPrologue.class)"));
    assert!(out.contains("static int enter(HandleCell cell)"));
    assert!(out.contains("CEntryPointActions.enterAttachThread(cell.isolate(), true)"));
    assert!(out.contains("ListenerCallbacks.free(__cell.token());"));
    assert!(out.contains("ListenerCallbacks.on_value(__cell.token(), value)"));
    assert!(out.contains("static void ___ListenerVTable_on_text(HandleCell __cell, org.graalvm.word.PointerBase text_ptr, long text_len) {"));
    assert!(out.contains("static boolean ___ListenerVTable_on_batch(HandleCell __cell, org.graalvm.word.PointerBase values_ptr, long values_len) {"));
    assert!(out.contains("static FfiBuf_u8 ___ListenerVTable_describe(uint64_t handle) {"));
    // registration is lazy: the `@CContext` class initializes at image build
    // time, where the C library is not linked yet, so it cannot run in `<clinit>`
    assert!(!out.contains("    static {\n        Lib.boltffi_ni_register_callbacks();"));
    assert!(out.contains("    static void ensureCallbacksRegistered() {"));
    assert!(out.contains("            Lib.boltffi_ni_register_callbacks();"));
    assert!(out.contains(
        "    static long newHandleCell(long token) {\n        ensureCallbacksRegistered();"
    ));
    // a custom prologue needs an explicit bailout for every non-`int`, non-`void` upcall
    assert!(out.contains(
        "CEntryPointOptions(prologue = AttachPrologue.class, prologueBailout = BailoutLong.class)"
    ));
    assert!(out.contains(
        "CEntryPointOptions(prologue = AttachPrologue.class, prologueBailout = BailoutBoolean.class)"
    ));
    assert!(out.contains("static boolean bailout(int prologueResult) { return false; }"));
    // `int` and `void` upcalls keep the automatic bailout
    assert!(out.contains(
        "CEntryPointOptions(prologue = AttachPrologue.class)\n    static int ___ListenerVTable_on_value("
    ));
    // downcall passes the callback token through a stack BoltFFICallbackHandle
    assert!(out.contains("static int boltffi_function_demo_subscribe(long listener) {"));
    assert!(out.contains("CallbackHandle __cb_listener = StackValue.get(CallbackHandle.class);"));
    // C side: slot shims forward to exported Java entry points, vtable registered once
    assert!(out.contains("extern void boltffi_ni_java_"));
    assert!(out.contains("(BoltFFINativeImageHandleCell *)(uintptr_t)handle"));
    assert!(out.contains("void boltffi_ni_register_callbacks(void) {"));
    insta::assert_snapshot!(out);
}

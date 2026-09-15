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
    assert!(out.contains("Collections.singletonList(\"<demo.h>\")"));
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

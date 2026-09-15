use std::{fs, path::Path, process::Command};

fn run(command: &mut Command) {
    let output = command.output().expect("run fixture command");
    assert!(
        output.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn opted_in_dependency_exports_link_from_a_static_archive() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let root = repository
        .join("target")
        .join(format!("dependency-exports-{}", std::process::id()));
    fs::create_dir_all(root.join("dependency/src")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        r#"
[package]
name = "embedding"
version = "0.0.0"
edition = "2024"
[workspace]
[lib]
crate-type = ["staticlib"]
[dependencies]
export_dependency = { path = "dependency" }
"#,
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub use export_dependency::answer;\n",
    )
    .unwrap();
    let dependency_manifest = format!(
        r#"
[package]
name = "export_dependency"
version = "0.0.0"
edition = "2024"
[package.metadata.boltffi]
emit_bindings = true
[dependencies]
boltffi = {{ path = {:?}, default-features = false }}
"#,
        repository.join("boltffi")
    );
    fs::write(root.join("dependency/Cargo.toml"), &dependency_manifest).unwrap();
    fs::write(
        root.join("dependency/src/lib.rs"),
        "#[boltffi::export]\npub fn answer() -> u64 { 42 }\n",
    )
    .unwrap();
    let target = root.join("target");
    run(Command::new(env!("CARGO"))
        .args(["build", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target));
    fs::write(
        root.join("main.rs"),
        r#"
unsafe extern "C" { fn boltffi_function_export_dependency_answer() -> u64; }
fn main() { assert_eq!(unsafe { boltffi_function_export_dependency_answer() }, 42); }
"#,
    )
    .unwrap();
    let executable = root.join("consumer");
    let mut linker = Command::new("rustc");
    linker
        .arg(root.join("main.rs"))
        .arg("-L")
        .arg(format!("native={}", target.join("debug").display()))
        .args(["-l", "static=embedding", "-o"])
        .arg(&executable);
    run(&mut linker);
    run(&mut Command::new(executable));
    fs::write(
        root.join("dependency/Cargo.toml"),
        dependency_manifest.replace("[package.metadata.boltffi]\nemit_bindings = true\n", ""),
    )
    .unwrap();
    run(Command::new(env!("CARGO"))
        .args(["build", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target));
    let output = linker.output().unwrap();
    assert!(
        !output.status.success(),
        "dependency exports must remain opt-in"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("boltffi_function_export_dependency_answer")
    );
    fs::remove_dir_all(root).unwrap();
}

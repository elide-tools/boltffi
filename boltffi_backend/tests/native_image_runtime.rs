//! Gated end-to-end GraalVM Native Image test for the Java target.
//!
//! The test writes a scratch `staticlib` fixture crate, renders Native Image
//! Java bindings for the same source, compiles the generated C shim into the
//! archive, builds a native image from the generated Java plus a driver, and
//! runs it. It is skipped when `native-image` cannot be discovered.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use boltffi_ast::PackageInfo;
use boltffi_backend::{
    CoverageMode,
    target::java::{JavaHost, JvmBinding},
};
use boltffi_binding::{Native, lower};

const JAVA_PACKAGE: &str = "com.boltffi.fixture";
const LIBRARY_NAME: &str = "boltffi_ni_fixture";

const RUST_SOURCE: &str = r##"
use boltffi::*;

/// A blittable point record.
#[repr(C)]
#[data]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// Fixture error carrying a code and a message.
#[error]
#[derive(Clone, Debug)]
pub struct AppError {
    /// Numeric error code.
    pub code: i32,
    /// Human readable message.
    pub message: String,
}

impl std::fmt::Display for AppError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

/// Round trips a signed 32-bit integer.
#[export]
pub fn round_trip_i32(value: i32) -> i32 {
    value
}

/// Negates a boolean.
#[export]
pub fn flip_bool(value: bool) -> bool {
    !value
}

/// Doubles a double.
#[export]
pub fn double_f64(value: f64) -> f64 {
    value * 2.0
}

/// Round trips an unsigned 64-bit integer.
#[export]
pub fn round_trip_u64(value: u64) -> u64 {
    value
}

/// Translates a direct record by an offset.
#[export]
pub fn translate_point(point: Point, dx: f64, dy: f64) -> Point {
    Point { x: point.x + dx, y: point.y + dy }
}

/// Upper-cases a string.
#[export]
pub fn shout(text: String) -> String {
    text.to_uppercase()
}

/// Divides two integers, failing on a zero divisor.
#[export]
pub fn checked_divide(numerator: i32, divisor: i32) -> Result<i32, AppError> {
    match divisor {
        0 => Err(AppError { code: 400, message: "division by zero".to_owned() }),
        _ => Ok(numerator / divisor),
    }
}

/// Describes a flag, failing when it is not set.
#[export]
pub fn describe(valid: bool) -> Result<String, AppError> {
    match valid {
        true => Ok("described".to_owned()),
        false => Err(AppError { code: 404, message: "undescribable".to_owned() }),
    }
}

/// Sums a slice of integers.
#[export]
pub fn sum_values(values: &[i32]) -> i64 {
    values.iter().map(|value| i64::from(*value)).sum()
}

/// A synchronous listener implemented on the JVM side.
#[export]
pub trait Listener {
    /// Transforms a value.
    fn on_value(&self, value: i32) -> i32;
    /// Observes a text notification.
    fn on_text(&self, text: &str);
}

/// Invokes the listener on the calling thread.
#[export]
pub fn drive_listener(listener: Box<dyn Listener>, value: i32) -> i32 {
    listener.on_text("sync");
    listener.on_value(value)
}

/// Invokes the listener from a foreign Rust thread.
#[export]
pub fn drive_listener_on_thread(listener: Box<dyn Listener + Send>, value: i32) -> i32 {
    std::thread::spawn(move || {
        listener.on_text("thread");
        listener.on_value(value)
    })
    .join()
    .unwrap_or(-1)
}
"##;

const MAIN_JAVA: &str = r#"package com.boltffi.fixture;

/** Exercises every generated Native Image binding shape. */
public final class Main {
    private static int failures = 0;

    private Main() {}

    private static void check(String name, boolean passed) {
        if (passed) {
            System.out.println("OK " + name);
        } else {
            failures += 1;
            System.out.println("FAIL " + name);
        }
    }

    /** Records what the Rust side reported back. */
    static final class Recorder implements Listener {
        volatile String text = null;
        volatile String thread = null;

        @Override
        public int onValue(int value) {
            this.thread = Thread.currentThread().getName();
            return value + 1;
        }

        @Override
        public void onText(String text) {
            this.text = text;
        }
    }

    public static void main(String[] args) {
        check("scalar_i32", Fixture.roundTripI32(-42) == -42);
        check("scalar_bool", Fixture.flipBool(false) && !Fixture.flipBool(true));
        check("scalar_f64", Fixture.doubleF64(1.5) == 3.0);
        check("scalar_u64", Fixture.roundTripU64(-1L) == -1L);

        Point translated = Fixture.translatePoint(new Point(1.0, 2.0), 0.5, -0.5);
        check("direct_record", translated.equals(new Point(1.5, 1.5)));

        check("string", "HELLO, WORLD".equals(Fixture.shout("Hello, world")));

        check("result_i32_ok", Fixture.checkedDivide(84, 2) == 42);
        boolean divideFailed = false;
        try {
            Fixture.checkedDivide(1, 0);
        } catch (AppError failure) {
            divideFailed = failure.code() == 400 && "division by zero".equals(failure.message());
        }
        check("result_i32_err", divideFailed);

        check("result_string_ok", "described".equals(Fixture.describe(true)));
        boolean describeFailed = false;
        try {
            Fixture.describe(false);
        } catch (AppError failure) {
            describeFailed = failure.code() == 404 && "undescribable".equals(failure.message());
        }
        check("result_string_err", describeFailed);

        check("vector_sum", Fixture.sumValues(new int[] {1, 2, 3, 4}) == 10L);

        Recorder sync = new Recorder();
        int syncResult = Fixture.driveListener(sync, 20);
        check("callback_sync", syncResult == 21 && "sync".equals(sync.text));

        Recorder foreign = new Recorder();
        int foreignResult = Fixture.driveListenerOnThread(foreign, 30);
        check(
            "callback_foreign_thread",
            foreignResult == 31
                && "thread".equals(foreign.text)
                && foreign.thread != null
                && !foreign.thread.equals(Thread.currentThread().getName())
        );

        if (failures != 0) {
            System.exit(1);
        }
    }
}
"#;

/// Every `OK <case>` line the driver must print.
const EXPECTED_CASES: &[&str] = &[
    "scalar_i32",
    "scalar_bool",
    "scalar_f64",
    "scalar_u64",
    "direct_record",
    "string",
    "result_i32_ok",
    "result_i32_err",
    "result_string_ok",
    "result_string_err",
    "vector_sum",
    "callback_sync",
    "callback_foreign_thread",
];

/// A discovered GraalVM Native Image toolchain.
struct Toolchain {
    home: PathBuf,
    native_image: PathBuf,
}

impl Toolchain {
    fn discover() -> Option<Self> {
        let from_home = |variable: &str| {
            let home = PathBuf::from(std::env::var_os(variable)?);
            let candidate = home.join("bin").join("native-image");
            candidate.is_file().then_some(candidate)
        };
        let native_image = from_home("GRAALVM_HOME")
            .or_else(|| from_home("JAVA_HOME"))
            .or_else(Self::from_path)?;
        let home = native_image.parent()?.parent()?.to_path_buf();
        Some(Self { home, native_image })
    }

    fn from_path() -> Option<PathBuf> {
        let paths = std::env::var_os("PATH")?;
        std::env::split_paths(&paths)
            .map(|directory| directory.join("native-image"))
            .find(|candidate| candidate.is_file())
    }

    fn javac(&self) -> PathBuf {
        self.home.join("bin").join("javac")
    }

    fn builder_classpath(&self) -> std::ffi::OsString {
        let builder = self.home.join("lib").join("svm").join("builder");
        let mut classpath = std::ffi::OsString::from(builder.join("svm-guest-staging.jar"));
        classpath.push(":");
        classpath.push(builder.join("svm-shared.jar"));
        classpath
    }
}

/// The scratch directory tree the test builds everything in.
struct Fixture {
    root: PathBuf,
    crate_dir: PathBuf,
    generated: PathBuf,
    native: PathBuf,
    classes: PathBuf,
    keep: bool,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "boltffi-native-image-{}-{nonce}",
            std::process::id()
        ));
        let fixture = Self {
            crate_dir: root.join("fixture"),
            generated: root.join("generated"),
            native: root.join("native"),
            classes: root.join("classes"),
            root,
            keep: std::env::var_os("BOLTFFI_NI_KEEP").is_some(),
        };
        for directory in [
            &fixture.root,
            &fixture.crate_dir,
            &fixture.generated,
            &fixture.native,
            &fixture.classes,
        ] {
            fs::create_dir_all(directory).expect("create Native Image fixture directory");
        }
        fixture
    }

    fn repository(&self) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .to_path_buf()
    }

    /// Writes the scratch `staticlib` crate and builds it.
    fn build_rust_archive(&self) -> PathBuf {
        let repository = self.repository();
        let manifest = format!(
            "[package]\n\
             name = \"{LIBRARY_NAME}\"\n\
             version = \"0.0.0\"\n\
             edition = \"2024\"\n\
             publish = false\n\
             \n\
             [workspace]\n\
             \n\
             [lib]\n\
             crate-type = [\"staticlib\"]\n\
             \n\
             [dependencies]\n\
             boltffi = {{ path = \"{}\" }}\n",
            repository.join("boltffi").display()
        );
        fs::write(self.crate_dir.join("Cargo.toml"), manifest).expect("write fixture manifest");
        let toolchain = repository.join("rust-toolchain.toml");
        if toolchain.is_file() {
            fs::copy(toolchain, self.crate_dir.join("rust-toolchain.toml"))
                .expect("copy rust toolchain pin");
        }
        fs::create_dir_all(self.crate_dir.join("src")).expect("create fixture source directory");
        fs::write(self.crate_dir.join("src").join("lib.rs"), RUST_SOURCE)
            .expect("write fixture source");

        let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
        cargo
            .current_dir(&self.crate_dir)
            .args(["build", "--release"]);
        self.run(&mut cargo, "fixture staticlib build");
        self.crate_dir
            .join("target")
            .join("release")
            .join(format!("lib{LIBRARY_NAME}.a"))
    }

    /// Renders the Native Image Java bindings for the fixture source.
    fn generate_java(&self) {
        let file = syn::parse_str(RUST_SOURCE).expect("valid Native Image fixture source");
        let scanned = boltffi_scan::scan_file(file, PackageInfo::new(LIBRARY_NAME, None))
            .expect("Native Image fixture should scan");
        let bindings = lower::<Native>(&scanned).expect("Native Image fixture should lower");
        let host = JavaHost::new(JAVA_PACKAGE, "Fixture")
            .expect("Java host")
            .binding(JvmBinding::NativeImage)
            .desktop_fallback_library(LIBRARY_NAME)
            .expect("static library name");
        let output = host
            .render_with_coverage(&bindings, CoverageMode::Complete)
            .expect("Native Image target should render");
        for generated in output.files() {
            let destination = self.generated.join(generated.path().as_path());
            fs::create_dir_all(destination.parent().expect("generated file parent"))
                .expect("create generated directory");
            fs::write(destination, generated.contents()).expect("write generated file");
        }
    }

    fn package_directory(&self) -> PathBuf {
        self.generated.join(JAVA_PACKAGE.replace('.', "/"))
    }

    fn headers(&self) -> PathBuf {
        self.generated.join("native_image")
    }

    /// Compiles the generated C shim into the Rust archive.
    fn build_static_library(&self, archive: &Path) {
        let headers = self.headers();
        let object = self.root.join("glue.o");
        let mut compiler = Command::new("cc");
        compiler
            .current_dir(&self.root)
            .args(["-std=c11", "-O2", "-c"])
            .arg(headers.join("native_image_glue.c"))
            .arg("-I")
            .arg(&headers)
            .arg("-o")
            .arg(&object);
        self.run(&mut compiler, "Native Image glue compilation");

        let library = self.native.join(format!("lib{LIBRARY_NAME}.a"));
        fs::copy(archive, &library).expect("stage the Rust static library");
        let mut archiver = Command::new("ar");
        archiver
            .current_dir(&self.root)
            .arg("rcs")
            .arg(&library)
            .arg(&object);
        self.run(&mut archiver, "static library append");
    }

    /// Compiles the generated Java plus the driver.
    fn compile_java(&self, toolchain: &Toolchain) {
        fs::write(self.package_directory().join("Main.java"), MAIN_JAVA)
            .expect("write Native Image driver");
        let mut sources = fs::read_dir(self.package_directory())
            .expect("read generated Java package")
            .map(|entry| entry.expect("generated Java entry").path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "java")
            })
            .collect::<Vec<_>>();
        sources.sort();

        let mut compiler = Command::new(toolchain.javac());
        compiler
            .current_dir(&self.root)
            // `--release` resolves against `ct.sym`, which omits the GraalVM
            // modules the generated bridge imports, so pin source and target
            // instead and compile against this JDK's own module graph.
            .args(["-encoding", "UTF-8", "-source", "25", "-target", "25"])
            .arg("-d")
            .arg(&self.classes)
            .arg("-cp")
            .arg(toolchain.builder_classpath())
            .args(sources);
        self.run(&mut compiler, "Java compilation");
    }

    /// Builds and runs the native image.
    fn build_and_run_image(&self, toolchain: &Toolchain) -> String {
        let mut classpath = std::ffi::OsString::from(&self.classes);
        classpath.push(":");
        classpath.push(toolchain.builder_classpath());

        let binary = self.root.join("ni-fixture");
        let mut builder = Command::new(&toolchain.native_image);
        builder
            .current_dir(&self.root)
            .arg("-cp")
            .arg(&classpath)
            .args([
                "--add-exports",
                "org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c.function=ALL-UNNAMED",
                "--add-exports",
                "org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c=ALL-UNNAMED",
                "--add-exports",
                "org.graalvm.nativeimage.shared/com.oracle.svm.shared=ALL-UNNAMED",
            ])
            .arg(format!("-H:CLibraryPath={}", self.native.display()))
            .arg(format!("-Dboltffi.include={}", self.headers().display()))
            .arg("-o")
            .arg(&binary)
            .arg(format!("{JAVA_PACKAGE}.Main"));
        self.run(&mut builder, "native image build");

        let mut program = Command::new(&binary);
        program.current_dir(&self.root);
        self.run(&mut program, "native image execution")
    }

    fn run(&self, command: &mut Command, operation: &str) -> String {
        let output = command
            .output()
            .unwrap_or_else(|failure| panic!("{operation} could not start: {failure}"));
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "{operation} failed ({})\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status
        );
        format!("{stdout}{stderr}")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        match self.keep {
            true => eprintln!("kept Native Image fixture at {}", self.root.display()),
            false => drop(fs::remove_dir_all(&self.root)),
        }
    }
}

#[test]
fn generated_native_image_bindings_round_trip() {
    let Some(toolchain) = Toolchain::discover() else {
        println!("skipping: native-image not found");
        return;
    };
    let fixture = Fixture::new();
    fixture.generate_java();
    let archive = fixture.build_rust_archive();
    fixture.build_static_library(&archive);
    fixture.compile_java(&toolchain);
    let output = fixture.build_and_run_image(&toolchain);

    for case in EXPECTED_CASES {
        assert!(
            output.contains(&format!("OK {case}")),
            "native image run did not report `OK {case}`\noutput:\n{output}"
        );
    }
    assert!(
        !output.contains("FAIL "),
        "native image run reported failures\noutput:\n{output}"
    );
}

use std::path::{Path, PathBuf};
use std::process::Command;

use boltffi_backend::target::jvm::NativeLibraries;

use crate::cli::{CliError, Result};
use crate::config::Config;
use crate::pack::{format_command_for_log, print_verbose_detail};
use crate::reporter::Step;
use crate::target::JavaHostTarget;

use super::link::{JvmBuildArtifacts, JvmNativeLinkInput, resolve_jvm_native_link_input};
use super::plan::JvmPackagingTarget;

/// Subdirectory (relative to the Java output directory) where the Native
/// Image bridge renders the C header, the pointer-ABI glue source, and the
/// glue header. Written by the generator, read here for packaging.
pub(crate) const NATIVE_IMAGE_GENERATED_DIR: &str = "native_image";

/// Fixed filename of the generated C header. Unlike the JNI bridge's
/// `<artifact>.h`, the Native Image bridge always emits this name.
pub(crate) const NATIVE_IMAGE_HEADER_FILENAME: &str = "boltffi.h";

/// Fixed filename of the pointer-ABI C shim source.
pub(crate) const NATIVE_IMAGE_GLUE_SOURCE_FILENAME: &str = "native_image_glue.c";

/// Fixed filename of the pointer-ABI C shim header.
pub(crate) const NATIVE_IMAGE_GLUE_HEADER_FILENAME: &str = "native_image_glue.h";

/// Exact contents written to every generated `native-image.properties` file.
///
/// Content never depends on the Java package or artifact name; only the
/// file's path (under `META-INF/native-image/<group>/<artifact>/`) does.
pub(crate) const NATIVE_IMAGE_PROPERTIES: &str = "Args = -H:CLibraryPath=${.}/../../../../native \\
       -Dboltffi.include=${.}/../../../../native \\
       --add-exports org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c.function=ALL-UNNAMED \\
       --add-exports org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c=ALL-UNNAMED \\
       --add-exports org.graalvm.nativeimage.shared/com.oracle.svm.shared=ALL-UNNAMED
";

/// Resolved output paths for one Native Image packaging run.
///
/// Unlike the JNI layout, this is flat (no per-host `native/<host>/`
/// subdirectory): GraalVM `native-image` builds for the host running it, so
/// `boltffi pack java` with `binding = "native-image"` supports exactly one
/// configured host target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeImageOutputLayout {
    pub(crate) native_dir: PathBuf,
    pub(crate) archive_path: PathBuf,
    pub(crate) header_path: PathBuf,
    pub(crate) glue_header_path: PathBuf,
    pub(crate) properties_path: PathBuf,
}

impl NativeImageOutputLayout {
    /// Resolves the output layout for a Native Image packaging run.
    ///
    /// `java_package` is the Maven-style `group` and `artifact_name` is the
    /// `artifact` segment of `META-INF/native-image/<group>/<artifact>/`.
    pub(crate) fn resolve(
        java_output: &Path,
        java_package: &str,
        artifact_name: &str,
        host_target: JavaHostTarget,
    ) -> Result<Self> {
        let libraries = NativeLibraries::from_artifact(artifact_name).map_err(|error| {
            CliError::CommandFailed {
                command: format!("resolve JVM library names: {error}"),
                status: None,
            }
        })?;
        let native_dir = java_output.join("native");
        let archive_path = native_dir
            .join(host_target.static_library_filename(libraries.desktop_fallback().as_str()));

        Ok(Self {
            header_path: native_dir.join(NATIVE_IMAGE_HEADER_FILENAME),
            glue_header_path: native_dir.join(NATIVE_IMAGE_GLUE_HEADER_FILENAME),
            properties_path: java_output
                .join("META-INF")
                .join("native-image")
                .join(java_package)
                .join(artifact_name)
                .join("native-image.properties"),
            native_dir,
            archive_path,
        })
    }

    /// Enumerates every file this layout writes, for plan/output-list tests.
    #[cfg(test)]
    pub(crate) fn outputs(&self) -> Vec<&Path> {
        vec![
            self.archive_path.as_path(),
            self.header_path.as_path(),
            self.glue_header_path.as_path(),
            self.properties_path.as_path(),
        ]
    }
}

/// Result of packaging one Native Image host target: the static archive and
/// the headers copied alongside it.
#[derive(Debug, Clone)]
pub(crate) struct NativeImagePackagedOutput {
    pub(crate) host_target: JavaHostTarget,
    pub(crate) archive_path: PathBuf,
    pub(crate) header_path: PathBuf,
    pub(crate) glue_header_path: PathBuf,
}

/// Builds the Native Image static archive for one packaging target:
///
/// 1. Requires the selected crate to build a `staticlib` (clear error otherwise).
/// 2. Copies the Rust staticlib to `dist/java/native/lib<name>.a` (`<name>.lib` on MSVC).
/// 3. Compiles `native_image/native_image_glue.c` to an object file and appends it
///    to that same archive with `ar rcs` (`lib.exe` on MSVC).
/// 4. Copies the generated `boltffi.h` and `native_image_glue.h` alongside the archive.
pub(crate) fn build_native_image_native_library(
    config: &Config,
    packaging_target: &JvmPackagingTarget,
    build_artifacts: &JvmBuildArtifacts,
    step: &Step,
) -> Result<NativeImagePackagedOutput> {
    let cargo_context = &packaging_target.cargo_context;
    let host_target = cargo_context.host_target;
    let library = &cargo_context.library;

    if !library.builds_staticlib() {
        return Err(CliError::CommandFailed {
            command: format!(
                "targets.java.jvm.binding = \"native-image\" requires '{}' to build a staticlib; add `crate-type = [\"staticlib\"]` to [lib] in Cargo.toml",
                library.artifact_name()
            ),
            status: None,
        });
    }

    let link_input = resolve_jvm_native_link_input(
        &cargo_context.artifact_directory(),
        host_target,
        library,
        build_artifacts.static_library_filename.as_deref(),
    )?;
    let staticlib_path = match link_input {
        JvmNativeLinkInput::Staticlib(path) => path,
        JvmNativeLinkInput::Cdylib(_) => {
            return Err(CliError::CommandFailed {
                command: format!(
                    "no Rust staticlib artifact found for '{}'; native-image packaging requires a staticlib build",
                    library.artifact_name()
                ),
                status: None,
            });
        }
    };

    let layout = NativeImageOutputLayout::resolve(
        &config.java_jvm_output(),
        &config.java_package(),
        library.artifact_name(),
        host_target,
    )?;
    std::fs::create_dir_all(&layout.native_dir).map_err(|source| {
        CliError::CreateDirectoryFailed {
            path: layout.native_dir.clone(),
            source,
        }
    })?;

    std::fs::copy(&staticlib_path, &layout.archive_path).map_err(|source| {
        CliError::CopyFailed {
            from: staticlib_path.clone(),
            to: layout.archive_path.clone(),
            source,
        }
    })?;

    let generated_dir = config.java_jvm_output().join(NATIVE_IMAGE_GENERATED_DIR);
    let glue_source = generated_dir.join(NATIVE_IMAGE_GLUE_SOURCE_FILENAME);
    let glue_header = generated_dir.join(NATIVE_IMAGE_GLUE_HEADER_FILENAME);
    let generated_header = generated_dir.join(NATIVE_IMAGE_HEADER_FILENAME);
    for required in [&glue_source, &glue_header, &generated_header] {
        if !required.exists() {
            return Err(CliError::FileNotFound(required.clone()));
        }
    }

    let glue_object =
        compile_native_image_glue(packaging_target, &generated_dir, &glue_source, step)?;
    append_object_to_archive(packaging_target, &layout.archive_path, &glue_object, step)?;

    std::fs::copy(&generated_header, &layout.header_path).map_err(|source| {
        CliError::CopyFailed {
            from: generated_header.clone(),
            to: layout.header_path.clone(),
            source,
        }
    })?;
    std::fs::copy(&glue_header, &layout.glue_header_path).map_err(|source| {
        CliError::CopyFailed {
            from: glue_header.clone(),
            to: layout.glue_header_path.clone(),
            source,
        }
    })?;

    Ok(NativeImagePackagedOutput {
        host_target,
        archive_path: layout.archive_path,
        header_path: layout.header_path,
        glue_header_path: layout.glue_header_path,
    })
}

/// Writes `META-INF/native-image/<group>/<artifact>/native-image.properties`
/// with the fixed `Args =` contents documented on [`NATIVE_IMAGE_PROPERTIES`].
pub(crate) fn write_native_image_properties(layout: &NativeImageOutputLayout) -> Result<()> {
    if let Some(parent) = layout.properties_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CliError::CreateDirectoryFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    std::fs::write(&layout.properties_path, NATIVE_IMAGE_PROPERTIES).map_err(|source| {
        CliError::WriteFailed {
            path: layout.properties_path.clone(),
            source,
        }
    })
}

/// Compiles `native_image_glue.c` to an object file using the same compiler
/// discovery `compile_jni_library` uses (the host's discovered
/// clang/cc/cl/clang-cl program), with `include_dir` (the `native_image/`
/// directory) on the include path.
fn compile_native_image_glue(
    packaging_target: &JvmPackagingTarget,
    include_dir: &Path,
    glue_source: &Path,
    step: &Step,
) -> Result<PathBuf> {
    let toolchain = &packaging_target.toolchain;
    let msvc_style_compiler = toolchain.msvc_style_compiler();
    let object_extension = if msvc_style_compiler.is_some() {
        "obj"
    } else {
        "o"
    };
    let object_path = glue_source.with_extension(object_extension);

    let mut command = toolchain.linker_command();
    if let Some(compiler) = msvc_style_compiler {
        command.arg("/c");
        command.args(compiler.c11_atomics_flags());
        command.arg(format!("/I{}", include_dir.display()));
        command.arg(format!("/Fo{}", object_path.display()));
        command.arg(glue_source);
    } else {
        command
            .arg("-c")
            .arg("-fPIC")
            .arg(format!("-I{}", include_dir.display()))
            .arg("-o")
            .arg(&object_path)
            .arg(glue_source);
    }

    if step.is_verbose() {
        print_verbose_detail(&format!(
            "native-image glue compile command: {}",
            format_command_for_log(&command)
        ));
    }

    let status = command.status().map_err(|source| CliError::CommandFailed {
        command: format!("native-image glue compiler: {source}"),
        status: None,
    })?;

    if !status.success() {
        return Err(CliError::CommandFailed {
            command: format!(
                "native-image glue compiler failed to compile '{}'",
                glue_source.display()
            ),
            status: status.code(),
        });
    }

    Ok(object_path)
}

/// Appends `object_path` to the static archive at `archive_path` with
/// `ar rcs` (`lib.exe` on MSVC), so the archive carries both the Rust
/// staticlib's members and the glue shim's.
fn append_object_to_archive(
    packaging_target: &JvmPackagingTarget,
    archive_path: &Path,
    object_path: &Path,
    step: &Step,
) -> Result<()> {
    let toolchain = &packaging_target.toolchain;

    if toolchain.msvc_style_compiler().is_some() {
        let lib_program = which::which("lib.exe")
            .or_else(|_| which::which("lib"))
            .map_err(|_| CliError::CommandFailed {
                command: "no 'lib.exe' archiver found to build the native-image static archive"
                    .to_string(),
                status: None,
            })?;
        let combined_path = archive_path.with_extension("native_image_combined.lib");
        let mut command = Command::new(lib_program);
        command
            .arg(format!("/OUT:{}", combined_path.display()))
            .arg(archive_path)
            .arg(object_path);

        if step.is_verbose() {
            print_verbose_detail(&format!(
                "native-image archive command: {}",
                format_command_for_log(&command)
            ));
        }

        let status = command.status().map_err(|source| CliError::CommandFailed {
            command: format!("lib.exe: {source}"),
            status: None,
        })?;
        if !status.success() {
            return Err(CliError::CommandFailed {
                command: format!(
                    "lib.exe failed to append '{}' to '{}'",
                    object_path.display(),
                    archive_path.display()
                ),
                status: status.code(),
            });
        }

        std::fs::rename(&combined_path, archive_path).map_err(|source| CliError::CopyFailed {
            from: combined_path.clone(),
            to: archive_path.to_path_buf(),
            source,
        })?;

        return Ok(());
    }

    let ar_program = which::which("ar").map_err(|_| CliError::CommandFailed {
        command: "no 'ar' archiver found to build the native-image static archive".to_string(),
        status: None,
    })?;
    let mut command = Command::new(ar_program);
    command.arg("rcs").arg(archive_path).arg(object_path);

    if step.is_verbose() {
        print_verbose_detail(&format!(
            "native-image archive command: {}",
            format_command_for_log(&command)
        ));
    }

    let status = command.status().map_err(|source| CliError::CommandFailed {
        command: format!("ar: {source}"),
        status: None,
    })?;
    if !status.success() {
        return Err(CliError::CommandFailed {
            command: format!(
                "ar failed to append '{}' to '{}'",
                object_path.display(),
                archive_path.display()
            ),
            status: status.code(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{NATIVE_IMAGE_PROPERTIES, NativeImageOutputLayout};
    use crate::target::JavaHostTarget;

    #[test]
    fn layout_resolves_expected_output_files_on_unix_hosts() {
        let layout = NativeImageOutputLayout::resolve(
            &PathBuf::from("dist/java"),
            "com.example.boltffi",
            "demo_core",
            JavaHostTarget::LinuxX86_64,
        )
        .expect("resolve layout");

        assert_eq!(
            layout.archive_path,
            PathBuf::from("dist/java/native/libdemo_core.a")
        );
        assert_eq!(
            layout.header_path,
            PathBuf::from("dist/java/native/boltffi.h")
        );
        assert_eq!(
            layout.glue_header_path,
            PathBuf::from("dist/java/native/native_image_glue.h")
        );
        assert_eq!(
            layout.properties_path,
            PathBuf::from(
                "dist/java/META-INF/native-image/com.example.boltffi/demo_core/native-image.properties"
            )
        );

        let outputs = layout.outputs();
        assert_eq!(outputs.len(), 4);
        assert!(
            outputs
                .iter()
                .all(|path| !path.display().to_string().contains("_jni")),
            "native-image outputs must not include a JNI shared library: {outputs:?}"
        );
    }

    #[test]
    fn layout_uses_dot_lib_archive_name_on_windows() {
        let layout = NativeImageOutputLayout::resolve(
            &PathBuf::from("dist/java"),
            "com.example.boltffi",
            "demo_core",
            JavaHostTarget::WindowsX86_64,
        )
        .expect("resolve layout");

        assert_eq!(
            layout.archive_path,
            PathBuf::from("dist/java/native/demo_core.lib")
        );
    }

    #[test]
    fn properties_render_exact_expected_text() {
        let expected = "Args = -H:CLibraryPath=${.}/../../../../native \\
       -Dboltffi.include=${.}/../../../../native \\
       --add-exports org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c.function=ALL-UNNAMED \\
       --add-exports org.graalvm.nativeimage.guest.staging/com.oracle.svm.guest.staging.c=ALL-UNNAMED \\
       --add-exports org.graalvm.nativeimage.shared/com.oracle.svm.shared=ALL-UNNAMED
";
        assert_eq!(NATIVE_IMAGE_PROPERTIES, expected);
    }
}

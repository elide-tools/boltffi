//! Backend entry point for the Native Image bridge.
//!
//! A JVM target places `NativeImageBridge` above a `CBridge` in the backend
//! stack instead of `JniBridge`. The bridge builds the shared JVM contract in
//! Native Image mode, then renders `BoltFFINativeImage.java` and the
//! pointer-ABI glue shim.

use std::path::PathBuf;

use boltffi_binding::Native;

use crate::{
    bridge::{
        c::{self, CBridgeContract},
        jni::{JniBridgeContract, JvmClassPath},
    },
    core::{FilePath, GeneratedFile, GeneratedOutput, Result, bridge, contract::sealed},
};

use super::{contract::NativeImageContract, contract::Transition, template};

/// Policy for choosing `@CFunction` thread transitions.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum TransitionPolicy {
    /// Skip the native transition only when the crate can never call back into Java:
    /// no callbacks, closures, streams, or async functions.
    #[default]
    Automatic,
    /// Always transition to native state. Safe for every crate; slower per call.
    AlwaysTransition,
}

impl TransitionPolicy {
    pub(super) fn resolve(self, c_bridge: &CBridgeContract) -> Transition {
        match self {
            Self::AlwaysTransition => Transition::ToNative,
            Self::Automatic => match Self::can_reenter_java(c_bridge) {
                true => Transition::ToNative,
                false => Transition::NoTransition,
            },
        }
    }

    fn can_reenter_java(c_bridge: &CBridgeContract) -> bool {
        !c_bridge.callbacks().is_empty()
            || !c_bridge.streams().is_empty()
            || c_bridge.functions().iter().any(|function| {
                matches!(function.returns(), c::Type::FutureHandle)
                    || function.parameter_groups().iter().any(|group| {
                        matches!(
                            group,
                            c::ParameterGroup::Closure(_)
                                | c::ParameterGroup::ClosureReturn(_)
                                | c::ParameterGroup::Continuation(_)
                                | c::ParameterGroup::CallbackCompletion(_)
                        )
                    })
            })
    }
}

/// A Native Image bridge backend layered above the C ABI bridge.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct NativeImageBridge {
    owner: JvmClassPath,
    class: JvmClassPath,
    java_path: FilePath,
    glue_path: FilePath,
    library: String,
    transitions: TransitionPolicy,
}

impl NativeImageBridge {
    /// Java class name rendered by this bridge.
    pub const CLASS: &'static str = "BoltFFINativeImage";
    /// Default glue shim path relative to the generated output root.
    pub const GLUE_PATH: &'static str = "native_image/native_image_glue.c";

    /// Creates a Native Image bridge.
    ///
    /// `owner` is the host's native owner class name (kept for contract
    /// compatibility with JNI hosts). `library` is the static library name
    /// passed to `@CLibrary`, without `lib` prefix or extension.
    pub fn new(
        package: impl Into<String>,
        owner: impl Into<String>,
        java_path: impl Into<PathBuf>,
        glue_path: impl Into<PathBuf>,
        library: impl Into<String>,
    ) -> Result<Self> {
        let package = package.into();
        let library = library.into();
        if library.is_empty()
            || library.contains(['/', '\\'])
            || library.contains(char::is_whitespace)
        {
            return Err(crate::core::Error::UnsupportedTarget {
                target: super::NATIVE_IMAGE_BRIDGE,
                shape: "invalid static library name",
            });
        }
        Ok(Self {
            owner: JvmClassPath::new(package.clone(), owner)?,
            class: JvmClassPath::new(package, Self::CLASS)?,
            java_path: FilePath::new(java_path)?,
            glue_path: FilePath::new(glue_path)?,
            library,
            transitions: TransitionPolicy::default(),
        })
    }

    /// Creates the bridge a Java host uses: `Native` owner, package-relative Java path, default glue path.
    pub fn for_host(package: &str, library: &str) -> Result<Self> {
        let java_path = format!("{}/{}.java", package.replace('.', "/"), Self::CLASS);
        Self::new(package, "Native", java_path, Self::GLUE_PATH, library)
    }

    /// Selects the `@CFunction` transition policy.
    pub fn transitions(mut self, policy: TransitionPolicy) -> Self {
        self.transitions = policy;
        self
    }

    /// Returns the bridge-rendered Java class.
    pub fn class(&self) -> &JvmClassPath {
        &self.class
    }

    /// Returns the generated Java source path.
    pub fn java_path(&self) -> &FilePath {
        &self.java_path
    }

    /// Returns the generated glue shim path.
    pub fn glue_path(&self) -> &FilePath {
        &self.glue_path
    }

    /// Returns the static library name.
    pub fn library(&self) -> &str {
        &self.library
    }
}

impl bridge::BridgeBackend for NativeImageBridge {
    type Surface = Native;
    type Input = CBridgeContract;
    type Contract = JniBridgeContract;

    fn build_contract(&self, input: &Self::Input) -> Result<Self::Contract> {
        JniBridgeContract::for_native_image(self.owner.clone(), self.class.clone(), input)
    }

    fn render_bridge(
        &self,
        input: &Self::Input,
        contract: &Self::Contract,
    ) -> Result<GeneratedOutput> {
        let view = NativeImageContract::from_jni(
            contract,
            input,
            &self.glue_path,
            &self.library,
            self.transitions,
        )?;
        let java = template::render_java(&view)?;
        let glue = template::render_glue(&view)?;
        Ok(GeneratedOutput::new(
            vec![
                GeneratedFile::new(self.java_path.clone(), java),
                GeneratedFile::new(self.glue_path.clone(), glue),
            ],
            Vec::new(),
        ))
    }
}

impl sealed::BridgeBackend for NativeImageBridge {}

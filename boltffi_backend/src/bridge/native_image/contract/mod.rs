//! Native Image view of the JVM bridge contract.
//!
//! The JNI contract already decided which Java methods exist and what they
//! accept. This module decides how each of them reaches C from inside a
//! native image: which `@CFunction` to declare, whether a pointer-ABI shim is
//! needed, and which thread transition the call uses.

mod downcall;
mod java_type;
mod shim;

use crate::{
    bridge::{
        c::{self, HeaderInclude},
        jni::JniBridgeContract,
    },
    core::{FilePath, Result},
};

pub use downcall::{Downcall, LibParameter, Transition, Wrapper, WrapperParameter};
pub use java_type::JavaCType;
pub use shim::ShimFunction;

use super::bridge::TransitionPolicy;

/// Everything the Native Image templates need for one crate.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct NativeImageContract {
    package: String,
    class_name: String,
    library: String,
    header_file: String,
    glue_header: HeaderInclude,
    downcalls: Vec<Downcall>,
    shims: Vec<ShimFunction>,
    direct_records: Vec<String>,
    free_buffer: String,
}

impl NativeImageContract {
    /// Builds the Native Image contract from the JVM contract and the C bridge.
    pub fn from_jni(
        jvm: &JniBridgeContract,
        c_bridge: &c::CBridgeContract,
        glue_path: &FilePath,
        library: &str,
        policy: TransitionPolicy,
    ) -> Result<Self> {
        let class = jvm
            .native_image_class()
            .ok_or(crate::core::Error::BrokenBridgeContract {
                bridge: super::NATIVE_IMAGE_BRIDGE,
                invariant: "Native Image contract names the bridge-rendered class",
            })?;
        let transition = policy.resolve(c_bridge);
        let free_buffer_function = c_bridge.support().buffer_free()?;
        let support = [free_buffer_function]
            .into_iter()
            .map(|function| Downcall::from_c_function(function, Transition::NoTransition));
        let downcalls = support
            .chain(
                jvm.methods()
                    .iter()
                    .map(|method| Downcall::from_native_method(method, transition)),
            )
            .collect::<Result<Vec<_>>>()?;
        let free_buffer = downcalls
            .first()
            .map(|downcall| downcall.lib_name().as_str().to_owned())
            .unwrap_or_default();
        let direct_records = c_bridge
            .direct_records()
            .iter()
            .map(|record| record.name().to_owned())
            .collect();
        let shims = downcalls
            .iter()
            .map(ShimFunction::for_downcall)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        let header_file = c_bridge
            .header_path()
            .as_path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(crate::core::Error::BrokenBridgeContract {
                bridge: super::NATIVE_IMAGE_BRIDGE,
                invariant: "C header path has a file name",
            })?
            .to_owned();
        let package = class.as_java_path();
        let package = package
            .strip_suffix(class.class_name())
            .and_then(|prefix| prefix.strip_suffix('.'))
            .unwrap_or_default()
            .to_owned();
        Ok(Self {
            package,
            class_name: class.class_name().to_owned(),
            library: library.to_owned(),
            header_file,
            glue_header: HeaderInclude::from_files(glue_path, c_bridge.header_path())?,
            downcalls,
            shims,
            direct_records,
            free_buffer,
        })
    }

    /// Returns C typedef names of direct records, each needing a `@CStruct` size probe.
    pub fn direct_records(&self) -> &[String] {
        &self.direct_records
    }

    /// Returns the `Lib` symbol that releases an owned buffer through a pointer.
    pub fn free_buffer(&self) -> &str {
        &self.free_buffer
    }

    /// Returns the Java package of the generated class.
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Returns the generated Java class name.
    pub fn class_name(&self) -> &str {
        &self.class_name
    }

    /// Returns the static library name passed to `@CLibrary`.
    pub fn library(&self) -> &str {
        &self.library
    }

    /// Returns the C header file name included through the directives.
    pub fn header_file(&self) -> &str {
        &self.header_file
    }

    /// Returns the C header include used by the glue shim.
    pub fn glue_header(&self) -> &HeaderInclude {
        &self.glue_header
    }

    /// Returns every `@CFunction` downcall and its Java wrapper.
    pub fn downcalls(&self) -> &[Downcall] {
        &self.downcalls
    }

    /// Returns pointer-ABI shim functions the glue file defines.
    pub fn shims(&self) -> &[ShimFunction] {
        &self.shims
    }
}

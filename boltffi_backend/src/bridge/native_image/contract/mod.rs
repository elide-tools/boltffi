//! Native Image view of the JVM bridge contract.
//!
//! The JNI contract already decided which Java methods exist and what they
//! accept. This module decides how each of them reaches C from inside a
//! native image: which `@CFunction` to declare, whether a pointer-ABI shim is
//! needed, and which thread transition the call uses.

mod downcall;
mod java_type;
mod shim;
mod upcall;

use crate::{
    bridge::{
        c::{self, HeaderInclude},
        jni::JniBridgeContract,
    },
    core::{FilePath, Result},
};

pub use downcall::{Downcall, LibParameter, Transition, Wrapper, WrapperParameter};

/// Glue function that registers every callback vtable with Rust.
pub const REGISTER_CALLBACKS: &str = "boltffi_ni_register_callbacks";
pub use java_type::JavaCType;
pub use shim::ShimFunction;
pub use upcall::{CallbackUpcall, Trampoline};

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
    buffer_from_bytes: String,
    callbacks: Vec<CallbackUpcall>,
    glue_header_file: String,
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
        let buffer_from_bytes_function = c_bridge.support().buffer_from_bytes()?;
        let support = [free_buffer_function, buffer_from_bytes_function]
            .into_iter()
            .map(|function| Downcall::from_c_function(function, Transition::NoTransition));
        let create_handles = c_bridge
            .callbacks()
            .iter()
            .map(|callback| Downcall::from_c_function(callback.create_handle(), transition));
        let register_all = (!c_bridge.callbacks().is_empty())
            .then(|| {
                c::Function::new(REGISTER_CALLBACKS, Vec::new(), c::Type::Void)
                    .and_then(|function| Downcall::from_c_function(&function, transition))
            })
            .transpose()?;
        let mut downcalls = support
            .chain(create_handles)
            .chain(register_all.map(Ok))
            .chain(jvm.methods().iter().map(|method| {
                Downcall::from_native_method(method, transition, c_bridge.callbacks())
            }))
            .collect::<Result<Vec<_>>>()?;
        let free_buffer = downcalls[0].lib_name().as_str().to_owned();
        let buffer_from_bytes = downcalls[1].lib_name().as_str().to_owned();
        let callbacks = jvm
            .callbacks()
            .iter()
            .zip(c_bridge.callbacks())
            .zip(downcalls[2..2 + c_bridge.callbacks().len()].iter())
            .map(|((registration, callback), create)| {
                CallbackUpcall::new(registration, callback, create.lib_name().as_str())
            })
            .collect::<Result<Vec<_>>>()?;
        for registration in jvm.callbacks() {
            for method in registration.handle_methods() {
                downcalls.push(Downcall::from_handle_method(
                    method,
                    transition,
                    c_bridge.callbacks(),
                )?);
            }
        }
        if let Some(lifecycle) = jvm.callback_handle_lifecycle() {
            downcalls.push(Downcall::synthetic(Wrapper::lifecycle_clone(lifecycle)?));
            downcalls.push(Downcall::synthetic(Wrapper::lifecycle_release(lifecycle)?));
        }
        for writer in jvm.success_out_writers() {
            downcalls.push(Downcall::synthetic(Wrapper::success_out_writer(writer)?));
        }
        let direct_records = c_bridge
            .direct_records()
            .iter()
            .map(|record| record.name().to_owned())
            .collect();
        let mut shims = downcalls
            .iter()
            .map(|downcall| match downcall.extra_shim() {
                Some(shim) => Ok(Some(shim.clone())),
                None => ShimFunction::for_downcall(downcall),
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        shims.extend(
            callbacks
                .iter()
                .flat_map(|upcall| upcall.shims().iter().cloned()),
        );
        if !callbacks.is_empty() {
            let registrations = callbacks
                .iter()
                .map(|upcall| format!("    {}(&{});", upcall.register(), upcall.vtable()))
                .collect::<Vec<_>>()
                .join("\n");
            shims.push(ShimFunction::raw(
                c::Identifier::parse(REGISTER_CALLBACKS)?,
                format!("void {REGISTER_CALLBACKS}(void) {{\n{registrations}\n}}"),
            ));
        }
        let glue_header_file = glue_path
            .as_path()
            .with_extension("h")
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(crate::core::Error::BrokenBridgeContract {
                bridge: super::NATIVE_IMAGE_BRIDGE,
                invariant: "glue path has a file name",
            })?
            .to_owned();
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
            buffer_from_bytes,
            callbacks,
            glue_header_file,
        })
    }

    /// Returns the `Lib` symbol that copies bytes into an owned buffer through a pointer.
    pub fn buffer_from_bytes(&self) -> &str {
        &self.buffer_from_bytes
    }

    /// Returns Java-implemented callback traits.
    pub fn callbacks(&self) -> &[CallbackUpcall] {
        &self.callbacks
    }

    /// Returns whether any callback vtable must be registered at class init.
    pub fn registers_callbacks(&self) -> bool {
        !self.callbacks.is_empty()
    }

    /// Returns the glue function that registers every callback vtable.
    pub fn register_callbacks(&self) -> &str {
        REGISTER_CALLBACKS
    }

    /// Returns the glue header file name (typedefs shared by Java and the shim).
    pub fn glue_header_file(&self) -> &str {
        &self.glue_header_file
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

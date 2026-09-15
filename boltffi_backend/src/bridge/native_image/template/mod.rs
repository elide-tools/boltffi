//! Askama rendering for the Native Image bridge.

use askama::Template as AskamaTemplate;

use crate::core::{BackendError, Result};

use super::contract::{NativeImageContract, Trampoline, Wrapper as WrapperContract};

#[derive(AskamaTemplate)]
#[template(path = "bridge/native_image/native_image.java", escape = "none")]
struct JavaFile<'contract> {
    contract: &'contract NativeImageContract,
    wrappers: Vec<String>,
    trampolines: Vec<String>,
}

#[derive(AskamaTemplate)]
#[template(path = "bridge/native_image/trampoline.java", escape = "none")]
struct TrampolineTemplate<'trampoline> {
    trampoline: &'trampoline Trampoline,
}

#[derive(AskamaTemplate)]
#[template(path = "bridge/native_image/wrapper.java", escape = "none")]
struct Wrapper<'wrapper> {
    wrapper: &'wrapper WrapperContract,
}

#[derive(AskamaTemplate)]
#[template(path = "bridge/native_image/glue.c", escape = "none")]
struct Glue<'contract> {
    contract: &'contract NativeImageContract,
}

#[derive(AskamaTemplate)]
#[template(path = "bridge/native_image/glue.h", escape = "none")]
struct GlueHeader;

/// Renders the glue header shared by Java `@CStruct` probes and the shim.
pub(super) fn render_glue_header(_contract: &NativeImageContract) -> Result<String> {
    GlueHeader.render().map_err(BackendError::from)
}

/// Renders `BoltFFINativeImage.java`.
pub(super) fn render_java(contract: &NativeImageContract) -> Result<String> {
    let wrappers = contract
        .downcalls()
        .iter()
        .filter_map(|downcall| downcall.wrapper())
        .map(|wrapper| Wrapper { wrapper }.render().map_err(BackendError::from))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let trampolines = contract
        .callbacks()
        .iter()
        .flat_map(|callback| callback.trampolines())
        .map(|trampoline| {
            TrampolineTemplate { trampoline }
                .render()
                .map_err(BackendError::from)
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    JavaFile {
        contract,
        wrappers,
        trampolines,
    }
    .render()
    .map_err(BackendError::from)
}

/// Renders the pointer-ABI glue shim.
pub(super) fn render_glue(contract: &NativeImageContract) -> Result<String> {
    Glue { contract }.render().map_err(BackendError::from)
}

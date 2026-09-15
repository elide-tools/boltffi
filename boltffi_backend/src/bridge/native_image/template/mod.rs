//! Askama rendering for the Native Image bridge.

use askama::Template as AskamaTemplate;

use crate::core::{BackendError, Result};

use super::contract::{NativeImageContract, Wrapper as WrapperContract};

#[derive(AskamaTemplate)]
#[template(path = "bridge/native_image/native_image.java", escape = "none")]
struct JavaFile<'contract> {
    contract: &'contract NativeImageContract,
    wrappers: Vec<String>,
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

/// Renders `BoltFFINativeImage.java`.
pub(super) fn render_java(contract: &NativeImageContract) -> Result<String> {
    let wrappers = contract
        .downcalls()
        .iter()
        .filter_map(|downcall| downcall.wrapper())
        .map(|wrapper| Wrapper { wrapper }.render().map_err(BackendError::from))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    JavaFile { contract, wrappers }
        .render()
        .map_err(BackendError::from)
}

/// Renders the pointer-ABI glue shim.
pub(super) fn render_glue(contract: &NativeImageContract) -> Result<String> {
    Glue { contract }.render().map_err(BackendError::from)
}

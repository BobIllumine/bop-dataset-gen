//! The renderer boundary stays object-safe with concrete catalog and staging types.

use std::{collections::BTreeMap, num::NonZeroU32};

use bpp_assets::ResolvedAssetCatalog;
use bpp_core::{
    BackendInfo, BackendKind, RenderChannels, RenderError, RenderReport, RenderSceneRequest,
    RendererCapabilities,
};
use bpp_dataset::SceneStagingArea;
use bpp_render_api::Renderer;

struct ContractRenderer;

impl Renderer for ContractRenderer {
    fn info(&self) -> BackendInfo {
        BackendInfo::try_new(BackendKind::Wgpu, "test", None, BTreeMap::default()).unwrap()
    }

    fn capabilities(&self) -> RendererCapabilities {
        RendererCapabilities::new(
            RenderChannels::try_new(true, true, false).unwrap(),
            Some(NonZeroU32::new(2).unwrap()),
        )
    }

    fn prepare(&mut self, _assets: &ResolvedAssetCatalog) -> Result<(), RenderError> {
        Ok(())
    }

    fn render_scene(
        &mut self,
        _request: &RenderSceneRequest,
        _staging: &SceneStagingArea,
    ) -> Result<RenderReport, RenderError> {
        Err(RenderError::NotPrepared)
    }

    fn flush(&mut self) -> Result<(), RenderError> {
        Ok(())
    }
}

#[test]
fn renderer_is_an_object_safe_single_owner_boundary() {
    let mut renderer: Box<dyn Renderer> = Box::new(ContractRenderer);

    assert_eq!(renderer.info().kind(), BackendKind::Wgpu);
    renderer.flush().unwrap();
}

#[test]
fn renderer_owners_can_move_between_threads() {
    fn assert_send<T: Send>() {}
    assert_send::<Box<dyn Renderer>>();
}

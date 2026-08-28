//! Public object-safe renderer boundary and capability preflight contracts.

use std::{collections::BTreeMap, num::NonZeroU32};

use bpp_core::{
    BackendInfo, BackendKind, RenderChannels, RenderError, RenderReport, RenderSceneRequest,
    Renderer, RendererCapabilities,
};

struct ContractRenderer;

impl Renderer for ContractRenderer {
    type AssetCatalog = ();
    type StagingArea = ();

    fn info(&self) -> BackendInfo {
        BackendInfo::try_new(BackendKind::Wgpu, "test", None, BTreeMap::default()).unwrap()
    }

    fn capabilities(&self) -> RendererCapabilities {
        RendererCapabilities::new(
            RenderChannels::try_new(true, true, false).unwrap(),
            Some(NonZeroU32::new(2).unwrap()),
        )
    }

    fn prepare(&mut self, _assets: &Self::AssetCatalog) -> Result<(), RenderError> {
        Ok(())
    }

    fn render_scene(
        &mut self,
        _request: &RenderSceneRequest,
        _staging: &Self::StagingArea,
    ) -> Result<RenderReport, RenderError> {
        Err(RenderError::NotPrepared)
    }

    fn flush(&mut self) -> Result<(), RenderError> {
        Ok(())
    }
}

#[test]
fn renderer_is_an_object_safe_single_owner_boundary() {
    let mut renderer: Box<dyn Renderer<AssetCatalog = (), StagingArea = ()>> =
        Box::new(ContractRenderer);

    assert_eq!(renderer.info().kind(), BackendKind::Wgpu);
    renderer.prepare(&()).unwrap();
    renderer.flush().unwrap();
}

#[test]
fn capability_preflight_reports_channels_and_camera_limits_exactly() {
    let capabilities = ContractRenderer.capabilities();
    let unsupported_channels = RenderChannels::try_new(false, false, true).unwrap();

    assert!(matches!(
        capabilities.require(unsupported_channels, 1),
        Err(RenderError::UnsupportedCapabilities {
            required_channels,
            camera_count: 1,
            ..
        }) if required_channels == unsupported_channels
    ));
    assert!(matches!(
        capabilities.require(RenderChannels::try_new(true, false, false).unwrap(), 3),
        Err(RenderError::UnsupportedCapabilities {
            camera_count: 3,
            ..
        })
    ));
    capabilities
        .require(RenderChannels::try_new(true, true, false).unwrap(), 2)
        .expect("supported boundary is inclusive");
}

#[test]
fn retryability_is_explicit_and_never_inferred_from_error_text() {
    assert!(
        RenderError::WorkerCrashed {
            message: "exit 9".to_owned()
        }
        .is_retryable()
    );
    assert!(
        !RenderError::InvalidContent {
            message: "bad UV".to_owned()
        }
        .is_retryable()
    );
    assert!(!RenderError::NotPrepared.is_retryable());
}

//! Capability preflight and retry classification contracts.

use std::num::NonZeroU32;

use bop_core::{RenderChannels, RenderError, RendererCapabilities};

fn capabilities() -> RendererCapabilities {
    RendererCapabilities::new(
        RenderChannels::try_new(true, true, false).unwrap(),
        Some(NonZeroU32::new(2).unwrap()),
    )
}

#[test]
fn capability_preflight_reports_channels_and_camera_limits_exactly() {
    let capabilities = capabilities();
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

//! Public headless `wgpu` initialization contracts.

use std::error::Error;

use bpp_core::{WgpuAdapterPreference, WgpuDevicePreference, WgpuRendererConfig};
use bpp_render_wgpu::{WgpuInitializationError, WgpuRenderer};

fn config() -> WgpuRendererConfig {
    WgpuRendererConfig::try_new(
        WgpuDevicePreference::Automatic,
        WgpuAdapterPreference::HighPerformance,
        1024,
        false,
    )
    .unwrap()
}

#[test]
fn renderer_owner_is_send_and_initialization_errors_are_standard_errors() {
    fn assert_send<T: Send>() {}
    fn assert_error<T: Error>() {}
    assert_send::<WgpuRenderer>();
    assert_error::<WgpuInitializationError>();
}

#[test]
#[ignore = "manual hardware-dependent headless adapter smoke test"]
fn initializes_a_real_headless_device_and_reports_provenance() {
    let renderer = WgpuRenderer::try_new(config()).unwrap();
    let info = renderer.info();

    assert!(!info.implementation_version().trim().is_empty());
    assert!(
        info.device_name()
            .is_some_and(|name| !name.trim().is_empty())
    );
    assert_eq!(renderer.config(), config());
    assert!(renderer.capabilities().channels().rgb());
    assert!(renderer.capabilities().channels().depth());
    assert!(renderer.capabilities().channels().instance());
}

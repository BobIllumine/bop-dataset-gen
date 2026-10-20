//! Programmatic checked `wgpu` renderer configuration contracts.

use bop_core::{
    InvalidWgpuRendererConfigError, WgpuAdapterPreference, WgpuDevicePreference, WgpuRendererConfig,
};

#[test]
fn programmatic_wgpu_config_preserves_all_checked_options() {
    let config = WgpuRendererConfig::try_new(
        WgpuDevicePreference::Gpu,
        WgpuAdapterPreference::HighPerformance,
        2048,
        true,
    )
    .unwrap();

    assert_eq!(config.device_preference(), WgpuDevicePreference::Gpu);
    assert_eq!(
        config.adapter_preference(),
        WgpuAdapterPreference::HighPerformance
    );
    assert_eq!(config.shadow_map_size(), 2048);
    assert!(config.rgb_fxaa());
}

#[test]
fn programmatic_wgpu_config_rejects_zero_shadow_map_size() {
    assert_eq!(
        WgpuRendererConfig::try_new(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::Automatic,
            0,
            false,
        ),
        Err(InvalidWgpuRendererConfigError::ZeroShadowMapSize)
    );
}

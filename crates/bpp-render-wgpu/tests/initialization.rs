//! Public headless `wgpu` initialization contracts.

use std::{error::Error, sync::Arc};

use bpp_assets::{
    ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, VisualMesh, VisualPrimitive,
};
use bpp_core::{AssetId, WgpuAdapterPreference, WgpuDevicePreference, WgpuRendererConfig};
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

fn catalog() -> ResolvedAssetCatalog {
    let primitive = VisualPrimitive::try_new(
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0.0, 0.0, 1.0]; 3],
        vec![],
        vec![0, 1, 2],
        None,
    )
    .unwrap();
    let collision = ConvexPart::try_new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
    )
    .unwrap();
    let asset = Arc::new(
        NormalizedAsset::try_new(
            VisualMesh::try_new(vec![primitive]).unwrap(),
            vec![],
            vec![],
            vec![collision],
        )
        .unwrap(),
    );
    ResolvedAssetCatalog::try_new(vec![ResolvedAsset::new(
        AssetId::new("triangle").unwrap(),
        *blake3::hash(b"triangle").as_bytes(),
        asset,
    )])
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

#[test]
#[ignore = "manual hardware-dependent GPU upload smoke test"]
fn prepares_catalog_geometry_once_and_reports_exact_counts() {
    let mut renderer = WgpuRenderer::try_new(config()).unwrap();
    let catalog = catalog();
    let stats = renderer.prepare(&catalog).unwrap();

    assert_eq!(stats.asset_count(), 1);
    assert_eq!(stats.primitive_count(), 1);
    assert_eq!(stats.vertex_count(), 3);
    assert_eq!(stats.index_count(), 3);
    assert_eq!(
        renderer.prepared_catalog_fingerprint(),
        Some(catalog.fingerprint())
    );
}

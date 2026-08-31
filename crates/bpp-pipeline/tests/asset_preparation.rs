//! Public asset-preparation orchestration contracts.

use std::{fs, path::Path};

use bpp_core::RunConfig;
use bpp_pipeline::{AssetPreparationError, prepare_asset_catalog};

fn tetrahedron_stl() -> &'static str {
    "solid tetrahedron\n\
     facet normal 0 0 -1\nouter loop\nvertex 0 0 0\nvertex 0 1 0\nvertex 1 0 0\nendloop\nendfacet\n\
     facet normal 0 -1 0\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 0 1\nendloop\nendfacet\n\
     facet normal 1 1 1\nouter loop\nvertex 1 0 0\nvertex 0 1 0\nvertex 0 0 1\nendloop\nendfacet\n\
     facet normal -1 0 0\nouter loop\nvertex 0 1 0\nvertex 0 0 0\nvertex 0 0 1\nendloop\nendfacet\n\
     endsolid tetrahedron\n"
}

fn config(cache_path: &Path, source_path: &str) -> String {
    let fixture = include_str!("fixtures/serial_run.toml");
    fixture
        .replace(
            "cache_path = \"cache\"",
            &format!("cache_path = \"{}\"", cache_path.display()),
        )
        .replace(
            "source_path = \"cube.glb\"",
            &format!("source_path = {source_path:?}"),
        )
        .replace("source_unit_scale_m = 1.0", "source_unit_scale_m = 0.01")
}

#[test]
fn prepares_renderer_and_physics_catalogs_from_config_relative_stl() {
    let root = tempfile::tempdir().expect("temporary project root");
    let config_directory = root.path().join("config");
    fs::create_dir(&config_directory).expect("config directory");
    fs::write(config_directory.join("part.stl"), tetrahedron_stl()).expect("STL fixture");
    let parsed = RunConfig::from_toml(&config(&root.path().join("cache"), "part.stl"))
        .expect("fixture config is valid");

    let prepared = prepare_asset_catalog(parsed.plan(), &config_directory)
        .expect("config-relative STL prepares");

    assert_eq!(prepared.visual_assets().len(), 1);
    assert_eq!(prepared.physics_assets().len(), 1);
    let visual = prepared
        .visual_assets()
        .get(parsed.plan().assets()[0].id())
        .expect("visual asset is indexed");
    assert!(!visual.asset().convex_parts().is_empty());
    assert_eq!(
        prepared
            .physics_assets()
            .get(parsed.plan().assets()[0].id())
            .expect("physics asset is indexed")
            .part_count(),
        visual.asset().convex_parts().len()
    );
    assert_eq!(prepared.shared_physics_assets().len(), 1);
}

#[test]
fn rejects_unknown_asset_extensions_before_loading() {
    let root = tempfile::tempdir().expect("temporary project root");
    fs::write(root.path().join("part.obj"), b"not an obj").expect("source fixture");
    let parsed = RunConfig::from_toml(&config(&root.path().join("cache"), "part.obj"))
        .expect("fixture config is valid");

    let Err(error) = prepare_asset_catalog(parsed.plan(), root.path()) else {
        panic!("unsupported source extension must fail");
    };

    assert!(matches!(
        error,
        AssetPreparationError::UnsupportedFormat { .. }
    ));
}

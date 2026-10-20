//! Public resolved renderer-asset catalog contracts.

use std::sync::Arc;

use bop_assets::{
    ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, ResolvedAssetCatalogError,
    VisualMesh, VisualPrimitive,
};
use bop_core::AssetId;

fn asset(offset: f32) -> Arc<NormalizedAsset> {
    let visual = VisualPrimitive::try_new(
        vec![
            [offset, 0.0, 0.0],
            [offset + 1.0, 0.0, 0.0],
            [offset, 1.0, 0.0],
        ],
        vec![[0.0, 0.0, 1.0]; 3],
        vec![],
        vec![0, 1, 2],
        None,
    )
    .expect("visual triangle is valid");
    let collision = ConvexPart::try_new(
        vec![
            [offset, 0.0, 0.0],
            [offset + 1.0, 0.0, 0.0],
            [offset, 1.0, 0.0],
            [offset, 0.0, 1.0],
        ],
        vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
    )
    .expect("collision tetrahedron is valid");
    Arc::new(
        NormalizedAsset::try_new(
            VisualMesh::try_new(vec![visual]).expect("mesh is nonempty"),
            vec![],
            vec![],
            vec![collision],
        )
        .expect("asset references are valid"),
    )
}

fn fingerprint(source: &[u8]) -> [u8; 32] {
    *blake3::hash(source).as_bytes()
}

#[test]
fn catalog_sorts_assets_shares_geometry_and_locks_its_fingerprint() {
    let second_asset = asset(2.0);
    let first_asset = asset(0.0);
    let catalog = ResolvedAssetCatalog::try_new(vec![
        ResolvedAsset::new(
            AssetId::new("second").unwrap(),
            fingerprint(b"second"),
            Arc::clone(&second_asset),
        ),
        ResolvedAsset::new(
            AssetId::new("first").unwrap(),
            fingerprint(b"first"),
            Arc::clone(&first_asset),
        ),
    ])
    .expect("unique nonempty catalog is valid");

    assert_eq!(
        catalog
            .iter()
            .map(|entry| entry.asset_id().as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(Arc::ptr_eq(
        catalog
            .get(&AssetId::new("first").unwrap())
            .unwrap()
            .asset(),
        &first_asset
    ));
    assert_eq!(
        catalog.fingerprint(),
        "ca8a2aeb469d3eceb5059e89713d720124250fbaf0f210772caeb2b979c3b546"
    );
}

#[test]
fn catalog_rejects_empty_and_duplicate_ids_without_dropping_entries() {
    assert!(matches!(
        ResolvedAssetCatalog::try_new(vec![]),
        Err(ResolvedAssetCatalogError::Empty)
    ));
    let id = AssetId::new("cube").unwrap();
    assert!(matches!(
        ResolvedAssetCatalog::try_new(vec![
            ResolvedAsset::new(id.clone(), fingerprint(b"one"), asset(0.0)),
            ResolvedAsset::new(id.clone(), fingerprint(b"two"), asset(1.0)),
        ]),
        Err(ResolvedAssetCatalogError::DuplicateAssetId { asset_id }) if asset_id == id
    ));
}

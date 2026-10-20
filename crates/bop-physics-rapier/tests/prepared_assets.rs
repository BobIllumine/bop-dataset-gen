//! Public contracts for once-only Rapier collision-asset preparation.

use bop_assets::ConvexPart;
use bop_physics_rapier::{PhysicsAssetPreparationError, PreparedPhysicsAsset};

fn tetrahedron() -> ConvexPart {
    ConvexPart::try_new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        vec![0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3],
    )
    .expect("tetrahedron is checked")
}

#[test]
fn empty_prepared_parts_are_rejected() {
    let error = PreparedPhysicsAsset::try_new(&[]).expect_err("empty assets must be rejected");

    assert!(matches!(error, PhysicsAssetPreparationError::EmptyParts));
}

#[test]
fn unreferenced_off_plane_vertex_does_not_make_coplanar_indices_volumetric() {
    let part = ConvexPart::try_new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.5, 0.5, 1.0],
        ],
        vec![0, 1, 2, 0, 2, 3],
    )
    .expect("indexed surface is checked");

    let error = PreparedPhysicsAsset::try_new(&[part])
        .expect_err("only referenced coplanar vertices may form the hull");

    assert!(matches!(
        error,
        PhysicsAssetPreparationError::InvalidIndexedSurface { part_index: 0 }
    ));
}

#[test]
fn valid_parts_are_prepared_once_in_source_order() {
    let asset = PreparedPhysicsAsset::try_new(&[tetrahedron(), tetrahedron()])
        .expect("volumetric indexed parts prepare");

    assert_eq!(asset.part_count(), 2);
}

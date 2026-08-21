//! Behavioral tests for the public JSON wire contracts.

use bpp_core::{
    AssetId, AttemptId, CameraExtrinsics, CameraId, InstanceId, Meters, RigidTransform, SceneId,
    SchemaVersion,
};

fn assert_float_array_approx_eq<const N: usize>(actual: [f32; N], expected: [f32; N]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!((actual - expected).abs() < 1.0e-6);
    }
}

#[test]
fn numeric_identifier_wire_contracts_are_transparent_unsigned_integers() {
    assert_eq!(serde_json::to_string(&SceneId::new(9)).unwrap(), "9");
    assert_eq!(serde_json::to_string(&AttemptId::new(3)).unwrap(), "3");
    assert_eq!(serde_json::to_string(&CameraId::new(7)).unwrap(), "7");
    assert_eq!(
        serde_json::to_string(&InstanceId::new(42).unwrap()).unwrap(),
        "42"
    );

    assert_eq!(serde_json::from_str::<SceneId>("9").unwrap().get(), 9);
    assert_eq!(serde_json::from_str::<AttemptId>("3").unwrap().get(), 3);
    assert_eq!(serde_json::from_str::<CameraId>("7").unwrap().get(), 7);
    assert_eq!(serde_json::from_str::<InstanceId>("42").unwrap().get(), 42);
}

#[test]
fn asset_id_wire_contract_is_a_transparent_string() {
    assert_eq!(
        serde_json::to_string(&AssetId::new("widget-a").unwrap()).unwrap(),
        "\"widget-a\""
    );
    assert_eq!(
        serde_json::from_str::<AssetId>("\"widget-a\"")
            .unwrap()
            .as_str(),
        "widget-a"
    );
}

#[test]
fn meters_wire_contract_is_a_json_number() {
    assert_eq!(
        serde_json::to_string(&Meters::new(-2.5).unwrap()).unwrap(),
        "-2.5"
    );
    assert!((serde_json::from_str::<Meters>("1.25").unwrap().get() - 1.25).abs() < 1.0e-6);
}

#[test]
fn canonical_transform_wire_contract_preserves_field_order() {
    let transform = RigidTransform::try_new([1.5, -2.0, 0.0], [0.0, 0.0, 0.0, 1.0]).unwrap();

    assert_eq!(
        serde_json::to_string(&transform).unwrap(),
        "{\"translation\":[1.5,-2.0,0.0],\"rotation_xyzw\":[0.0,0.0,0.0,1.0]}"
    );

    let parsed = serde_json::from_str::<RigidTransform>(
        "{\"translation\":[1.5,-2.0,0.0],\"rotation_xyzw\":[0.0,0.0,0.0,1.0]}",
    )
    .unwrap();
    assert_float_array_approx_eq(parsed.translation().map(Meters::get), [1.5, -2.0, 0.0]);
    assert_float_array_approx_eq(parsed.rotation_xyzw(), [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn camera_extrinsics_wire_contract_wraps_the_checked_transform() {
    let transform = RigidTransform::try_new([1.5, -2.0, 0.0], [0.0, 0.0, 0.0, 1.0]).unwrap();
    let extrinsics = CameraExtrinsics::new(transform);

    assert_eq!(
        serde_json::to_string(&extrinsics).unwrap(),
        "{\"camera_from_world\":{\"translation\":[1.5,-2.0,0.0],\"rotation_xyzw\":[0.0,0.0,0.0,1.0]}}"
    );

    let parsed = serde_json::from_str::<CameraExtrinsics>(
        "{\"camera_from_world\":{\"translation\":[1.5,-2.0,0.0],\"rotation_xyzw\":[0.0,0.0,0.0,1.0]}}",
    )
    .unwrap();
    assert_eq!(parsed.camera_from_world(), transform);
}

#[test]
fn schema_version_wire_contract_is_integer_one() {
    assert_eq!(
        serde_json::to_string(&SchemaVersion::current()).unwrap(),
        "1"
    );
    assert_eq!(
        serde_json::from_str::<SchemaVersion>("1").unwrap(),
        SchemaVersion::current()
    );
}

#[test]
fn deserialization_rejects_invalid_checked_values() {
    assert!(serde_json::from_str::<InstanceId>("0").is_err());
    assert!(serde_json::from_str::<AssetId>("\" \\t\\n\"").is_err());
    assert!(serde_json::from_str::<SchemaVersion>("0").is_err());
    assert!(serde_json::from_str::<SchemaVersion>("2").is_err());
    assert!(
        serde_json::from_str::<RigidTransform>(
            "{\"translation\":[0.0,0.0,0.0],\"rotation_xyzw\":[0.0,0.0,0.0,0.0]}"
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<CameraExtrinsics>(
            "{\"camera_from_world\":{\"translation\":[0.0,0.0,0.0],\"rotation_xyzw\":[0.0,0.0,0.0,0.0]}}"
        )
        .is_err()
    );
}

#[test]
fn transform_deserialization_normalizes_and_canonicalizes_negative_quaternions() {
    let transform = serde_json::from_str::<RigidTransform>(
        "{\"translation\":[3.0,4.0,-0.0],\"rotation_xyzw\":[0.0,0.0,-2.0,-2.0]}",
    )
    .unwrap();

    assert_float_array_approx_eq(transform.translation().map(Meters::get), [3.0, 4.0, 0.0]);
    assert!(!transform.translation()[2].get().is_sign_negative());
    let rotation = transform.rotation_xyzw();
    assert!((rotation[2] - 0.707_106_77).abs() < 1.0e-6);
    assert!((rotation[3] - 0.707_106_77).abs() < 1.0e-6);
    assert!(rotation[2].is_sign_positive());
    assert!(rotation[3].is_sign_positive());
}

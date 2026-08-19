//! Behavioral tests for versioned camera calibration snapshots.

use bpp_core::{
    CameraExtrinsics, CameraId, CameraIntrinsics, CameraSnapshot, ImageSize,
    InvalidCameraSnapshotError, PositiveMeters, RigidTransform, SchemaVersion,
};

fn assert_same_float(actual: f32, expected: f32) {
    assert_eq!(actual.to_bits(), expected.to_bits());
}

fn assert_same_float_array(actual: [f32; 9], expected: [f32; 9]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert_same_float(actual, expected);
    }
}

fn valid_intrinsics() -> CameraIntrinsics {
    CameraIntrinsics::try_new([500.0, 0.25, 100.0, 0.0, 501.0, 200.0, 0.0, 0.0, 1.0])
        .expect("literal OpenCV calibration matrix is valid")
}

fn valid_snapshot() -> CameraSnapshot {
    CameraSnapshot::try_new(
        CameraId::new(7),
        ImageSize::try_new(640, 480).expect("nonzero image dimensions are valid"),
        valid_intrinsics(),
        CameraExtrinsics::new(RigidTransform::identity()),
        PositiveMeters::try_new(0.1).expect("positive near plane is valid"),
        PositiveMeters::try_new(10.0).expect("positive far plane is valid"),
    )
    .expect("near plane before far plane is valid")
}

#[test]
fn image_size_has_checked_object_wire_shape() {
    let image_size = ImageSize::try_new(640, 480).expect("nonzero dimensions are valid");

    assert_eq!(image_size.width(), 640);
    assert_eq!(image_size.height(), 480);
    assert_eq!(
        serde_json::to_string(&image_size).unwrap(),
        "{\"width\":640,\"height\":480}"
    );
    assert_eq!(
        serde_json::from_str::<ImageSize>("{\"width\":640,\"height\":480}").unwrap(),
        image_size
    );
}

#[test]
fn image_size_rejects_zero_resolution_and_unknown_fields() {
    assert!(ImageSize::try_new(0, 480).is_err());
    assert!(ImageSize::try_new(640, 0).is_err());
    assert!(serde_json::from_str::<ImageSize>("{\"width\":0,\"height\":480}").is_err());
    assert!(serde_json::from_str::<ImageSize>("{\"width\":640,\"height\":0}").is_err());
    assert!(
        serde_json::from_str::<ImageSize>("{\"width\":640,\"height\":480,\"extra\":true}").is_err()
    );
}

#[test]
fn camera_intrinsics_expose_the_complete_checked_opencv_matrix() {
    let intrinsics = valid_intrinsics();

    assert_same_float_array(
        intrinsics.k(),
        [500.0, 0.25, 100.0, 0.0, 501.0, 200.0, 0.0, 0.0, 1.0],
    );
    assert_same_float(intrinsics.fx(), 500.0);
    assert_same_float(intrinsics.fy(), 501.0);
    assert_same_float(intrinsics.skew(), 0.25);
    assert_same_float(intrinsics.cx(), 100.0);
    assert_same_float(intrinsics.cy(), 200.0);
    assert_eq!(
        serde_json::to_string(&intrinsics).unwrap(),
        "[500.0,0.25,100.0,0.0,501.0,200.0,0.0,0.0,1.0]"
    );
}

#[test]
fn camera_intrinsics_reject_invalid_matrix_values_but_allow_off_image_principal_points() {
    assert!(CameraIntrinsics::try_new([f32::NAN, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]).is_err());
    assert!(
        CameraIntrinsics::try_new([f32::INFINITY, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]).is_err()
    );
    assert!(CameraIntrinsics::try_new([0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]).is_err());
    assert!(CameraIntrinsics::try_new([1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0]).is_err());
    assert!(CameraIntrinsics::try_new([1.0, 0.0, 0.0, 2.0, 1.0, 0.0, 0.0, 0.0, 1.0]).is_err());
    assert!(CameraIntrinsics::try_new([1.0, 0.0, 0.0, -0.0, 1.0, 0.0, 0.0, 0.0, 1.0]).is_err());
    assert!(CameraIntrinsics::try_new([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.5]).is_err());
    assert!(
        serde_json::from_str::<CameraIntrinsics>("[0.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,1.0]").is_err()
    );

    let off_image =
        CameraIntrinsics::try_new([500.0, 0.0, -20.0, 0.0, 500.0, 9_999.0, 0.0, 0.0, 1.0])
            .expect("principal point need not lie within the image");
    assert_same_float(off_image.cx(), -20.0);
    assert_same_float(off_image.cy(), 9_999.0);
}

#[test]
fn camera_snapshot_has_exact_ordered_wire_shape_and_round_trips() {
    let snapshot = valid_snapshot();
    let expected = "{\"schema_version\":1,\"camera_id\":7,\"image_size\":{\"width\":640,\"height\":480},\"k\":[500.0,0.25,100.0,0.0,501.0,200.0,0.0,0.0,1.0],\"camera_from_world\":{\"camera_from_world\":{\"translation\":[0.0,0.0,0.0],\"rotation_xyzw\":[0.0,0.0,0.0,1.0]}},\"near_m\":0.1,\"far_m\":10.0}";

    assert_eq!(serde_json::to_string(&snapshot).unwrap(), expected);
    assert_eq!(
        serde_json::from_str::<CameraSnapshot>(expected).unwrap(),
        snapshot
    );
    assert_eq!(snapshot.schema_version(), SchemaVersion::current());
    assert_eq!(snapshot.camera_id(), CameraId::new(7));
    assert_eq!(snapshot.image_size().width(), 640);
    assert_eq!(snapshot.k(), valid_intrinsics());
    assert_eq!(
        snapshot.camera_from_world(),
        CameraExtrinsics::new(RigidTransform::identity())
    );
    assert_same_float(snapshot.near_m().get(), 0.1);
    assert_same_float(snapshot.far_m().get(), 10.0);
}

#[test]
fn camera_snapshot_rejects_depth_order_unknown_fields_and_nested_schema_version_two() {
    let near = PositiveMeters::try_new(1.0).unwrap();
    let far = PositiveMeters::try_new(2.0).unwrap();
    assert!(matches!(
        CameraSnapshot::try_new(
            CameraId::new(7),
            ImageSize::try_new(640, 480).unwrap(),
            valid_intrinsics(),
            CameraExtrinsics::new(RigidTransform::identity()),
            near,
            near,
        ),
        Err(InvalidCameraSnapshotError::NearNotBeforeFar { .. })
    ));
    assert!(matches!(
        CameraSnapshot::try_new(
            CameraId::new(7),
            ImageSize::try_new(640, 480).unwrap(),
            valid_intrinsics(),
            CameraExtrinsics::new(RigidTransform::identity()),
            far,
            near,
        ),
        Err(InvalidCameraSnapshotError::NearNotBeforeFar { .. })
    ));

    let valid = serde_json::to_string(&valid_snapshot()).unwrap();
    assert!(
        serde_json::from_str::<CameraSnapshot>(&valid.replacen(
            "\"schema_version\":1",
            "\"schema_version\":2",
            1
        ))
        .is_err()
    );
    assert!(
        serde_json::from_str::<CameraSnapshot>(&valid.replacen(
            "\"far_m\":10.0",
            "\"far_m\":10.0,\"extra\":true",
            1
        ))
        .is_err()
    );
    assert!(
        serde_json::from_str::<CameraSnapshot>(&valid.replacen(
            "\"near_m\":0.1",
            "\"near_m\":10.0",
            1
        ))
        .is_err()
    );
}

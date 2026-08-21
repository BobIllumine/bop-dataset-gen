//! Behavioral tests for public metre and rigid-transform contracts.

use bpp_core::{CameraExtrinsics, Meters, RigidTransform, z_up_camera_to_opencv};

const TOLERANCE: f32 = 1.0e-5;

fn metres(values: [f32; 3]) -> [Meters; 3] {
    values.map(|value| Meters::new(value).expect("finite metre value"))
}

fn assert_approx_eq(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= TOLERANCE,
        "expected {expected}, got {actual}"
    );
}

fn assert_point_approx_eq(actual: [Meters; 3], expected: [f32; 3]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert_approx_eq(actual.get(), expected);
    }
}

fn assert_rotation_approx_eq(actual: [f32; 4], expected: [f32; 4]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert_approx_eq(actual, expected);
    }
}

#[test]
fn metres_reject_nonfinite_values_and_preserve_negative_distances() {
    assert!(Meters::new(f32::NAN).is_none());
    assert!(Meters::new(f32::INFINITY).is_none());
    assert!(Meters::new(f32::NEG_INFINITY).is_none());

    let distance = Meters::new(-2.5).expect("negative translations are valid");

    assert_approx_eq(distance.get(), -2.5);
}

#[test]
fn identity_leaves_a_point_unchanged() {
    let identity = RigidTransform::identity();

    assert_eq!(identity.translation(), metres([0.0, 0.0, 0.0]));
    assert_rotation_approx_eq(identity.rotation_xyzw(), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(
        identity
            .transform_point(metres([-3.0, 2.0, 5.0]))
            .expect("identity result is finite"),
        metres([-3.0, 2.0, 5.0])
    );
}

#[test]
fn construction_normalizes_and_canonicalizes_quaternion_sign() {
    let transform = RigidTransform::try_new([0.0, 0.0, 0.0], [0.0, 0.0, -2.0, -2.0])
        .expect("finite nonzero quaternion");

    assert_rotation_approx_eq(
        transform.rotation_xyzw(),
        [0.0, 0.0, 0.5_f32.sqrt(), 0.5_f32.sqrt()],
    );
}

#[test]
fn construction_rejects_invalid_translation_and_rotation_components() {
    assert!(RigidTransform::try_new([f32::NAN, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]).is_none());
    assert!(RigidTransform::try_new([0.0, f32::INFINITY, 0.0], [0.0, 0.0, 0.0, 1.0]).is_none());
    assert!(RigidTransform::try_new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]).is_none());
    assert!(RigidTransform::try_new([0.0, 0.0, 0.0], [0.0, f32::NEG_INFINITY, 0.0, 1.0]).is_none());
}

#[test]
fn transform_point_applies_z_rotation_then_translation() {
    let transform =
        RigidTransform::try_new([1.0, 2.0, 3.0], [0.0, 0.0, 0.5_f32.sqrt(), 0.5_f32.sqrt()])
            .expect("valid transform");

    assert_point_approx_eq(
        transform
            .transform_point(metres([2.0, 0.0, 0.0]))
            .expect("finite transformed point"),
        [1.0, 4.0, 3.0],
    );
}

#[test]
fn inverse_round_trips_a_point() {
    let transform =
        RigidTransform::try_new([3.0, -2.0, 1.0], [0.0, 0.5_f32.sqrt(), 0.0, 0.5_f32.sqrt()])
            .expect("valid transform");
    let point = metres([2.0, -1.0, 4.0]);

    let inverse = transform.inverse().expect("finite inverse");

    let transformed = transform
        .transform_point(point)
        .expect("finite transformed point");

    assert_point_approx_eq(
        inverse
            .transform_point(transformed)
            .expect("finite round trip"),
        [2.0, -1.0, 4.0],
    );
}

#[test]
fn composition_applies_right_transform_first_and_is_noncommutative() {
    let translation =
        RigidTransform::try_new([1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]).expect("valid translation");
    let rotation =
        RigidTransform::try_new([0.0, 0.0, 0.0], [0.0, 0.0, 0.5_f32.sqrt(), 0.5_f32.sqrt()])
            .expect("valid rotation");
    let point = metres([1.0, 0.0, 0.0]);

    let rotate_after_translate = rotation.compose(translation).expect("finite composition");
    let translate_after_rotate = translation.compose(rotation).expect("finite composition");

    assert_point_approx_eq(
        rotate_after_translate
            .transform_point(point)
            .expect("finite transformed point"),
        [0.0, 2.0, 0.0],
    );
    assert_point_approx_eq(
        translate_after_rotate
            .transform_point(point)
            .expect("finite transformed point"),
        [1.0, 1.0, 0.0],
    );
}

#[test]
fn point_transformation_rejects_nonfinite_results() {
    let transform = RigidTransform::try_new([f32::MAX, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0])
        .expect("finite transform");

    assert!(
        transform
            .transform_point(metres([f32::MAX, 0.0, 0.0]))
            .is_none()
    );
}

#[test]
fn z_up_camera_basis_maps_points_to_opencv_coordinates() {
    let basis = z_up_camera_to_opencv();

    assert_point_approx_eq(
        basis
            .transform_point(metres([1.0, 2.0, 3.0]))
            .expect("finite basis result"),
        [1.0, -3.0, 2.0],
    );
}

#[test]
fn z_up_camera_basis_inverse_round_trips_a_point() {
    let point = metres([1.0, 2.0, 3.0]);
    let basis = z_up_camera_to_opencv();
    let inverse = basis.inverse().expect("finite basis inverse");

    let opencv_point = basis.transform_point(point).expect("finite basis result");

    assert_point_approx_eq(
        inverse
            .transform_point(opencv_point)
            .expect("finite inverse result"),
        [1.0, 2.0, 3.0],
    );
}

#[test]
fn camera_extrinsics_map_world_points_for_an_identity_z_up_camera_pose() {
    let world_from_z_up_camera = RigidTransform::try_new([0.0, -2.0, 1.0], [0.0, 0.0, 0.0, 1.0])
        .expect("finite camera pose");
    let extrinsics = CameraExtrinsics::try_from_world_from_z_up_camera(world_from_z_up_camera)
        .expect("finite camera extrinsics");

    assert_point_approx_eq(
        extrinsics
            .world_to_camera(metres([2.0, 1.0, 5.0]))
            .expect("finite camera point"),
        [2.0, -4.0, 3.0],
    );
}

#[test]
fn camera_extrinsics_round_trip_world_and_camera_points() {
    let camera_from_world =
        RigidTransform::try_new([0.0, 1.0, 2.0], [0.5_f32.sqrt(), 0.0, 0.0, 0.5_f32.sqrt()])
            .expect("finite camera transform");
    let extrinsics = CameraExtrinsics::new(camera_from_world);
    let world_point = metres([2.0, 1.0, 5.0]);

    let camera_point = extrinsics
        .world_to_camera(world_point)
        .expect("finite camera point");

    assert_point_approx_eq(
        extrinsics
            .camera_to_world(camera_point)
            .expect("finite world point"),
        [2.0, 1.0, 5.0],
    );
}

#[test]
fn camera_extrinsics_constructors_produce_equivalent_camera_transforms() {
    let world_from_z_up_camera = RigidTransform::try_new([0.0, -2.0, 1.0], [0.0, 0.0, 0.0, 1.0])
        .expect("finite camera pose");
    let explicit_camera_from_world =
        RigidTransform::try_new([0.0, 1.0, 2.0], [0.5_f32.sqrt(), 0.0, 0.0, 0.5_f32.sqrt()])
            .expect("finite camera transform");

    let explicit = CameraExtrinsics::new(explicit_camera_from_world);
    let derived = CameraExtrinsics::try_from_world_from_z_up_camera(world_from_z_up_camera)
        .expect("finite camera extrinsics");

    assert_point_approx_eq(
        explicit
            .camera_from_world()
            .transform_point(metres([2.0, 1.0, 5.0]))
            .expect("finite explicit result"),
        [2.0, -4.0, 3.0],
    );
    assert_point_approx_eq(
        derived
            .camera_from_world()
            .transform_point(metres([2.0, 1.0, 5.0]))
            .expect("finite derived result"),
        [2.0, -4.0, 3.0],
    );
    for (explicit, derived) in explicit
        .camera_from_world()
        .translation()
        .into_iter()
        .zip(derived.camera_from_world().translation())
    {
        assert_approx_eq(explicit.get(), derived.get());
    }
    assert_rotation_approx_eq(
        explicit.camera_from_world().rotation_xyzw(),
        derived.camera_from_world().rotation_xyzw(),
    );
    assert_point_approx_eq(
        explicit
            .world_from_camera()
            .expect("finite explicit inverse")
            .transform_point(metres([2.0, -4.0, 3.0]))
            .expect("finite explicit inverse point"),
        [2.0, 1.0, 5.0],
    );
    assert_point_approx_eq(
        derived
            .world_from_camera()
            .expect("finite derived inverse")
            .transform_point(metres([2.0, -4.0, 3.0]))
            .expect("finite derived inverse point"),
        [2.0, 1.0, 5.0],
    );
}

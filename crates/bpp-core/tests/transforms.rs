//! Behavioral tests for public metre and rigid-transform contracts.

use bpp_core::{Meters, RigidTransform};

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

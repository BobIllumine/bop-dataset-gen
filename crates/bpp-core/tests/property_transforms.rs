//! Bounded generative tests for rigid-transform numerical contracts.

mod property_support;

use bpp_core::{Meters, RigidTransform};
use proptest::prelude::*;

fn metres(point: [f32; 3]) -> [Meters; 3] {
    point.map(|value| Meters::new(value).expect("strategy emits finite point components"))
}

fn values(point: [Meters; 3]) -> [f32; 3] {
    point.map(Meters::get)
}

fn scaled_tolerance(left: [f32; 3], right: [f32; 3]) -> f32 {
    let scale = left
        .into_iter()
        .chain(right)
        .map(f32::abs)
        .fold(1.0_f32, f32::max);
    // A point composition performs two quaternion-vector products and two
    // additions. 128 ulps at the bounded result scale is conservative for
    // those operations while remaining below 0.0025 across these strategies.
    128.0 * f32::EPSILON * scale
}

fn points_approximately_equal(left: [f32; 3], right: [f32; 3]) -> bool {
    let tolerance = scaled_tolerance(left, right);
    left.into_iter()
        .zip(right)
        .all(|(left, right)| (left - right).abs() <= tolerance)
}

fn canonical_sign(rotation: [f32; 4]) -> bool {
    [rotation[3], rotation[0], rotation[1], rotation[2]]
        .into_iter()
        .find(|component| *component != 0.0)
        .is_some_and(f32::is_sign_positive)
}

fn no_negative_zero(value: &serde_json::Value) -> bool {
    value
        .as_array()
        .expect("transform component array")
        .iter()
        .all(|value| {
            let value = value.as_f64().expect("finite JSON number");
            value != 0.0 || value.to_bits() == 0.0_f64.to_bits()
        })
}

proptest! {
    #![proptest_config(property_support::suite_config(64))]

    #[test]
    fn construction_normalizes_and_canonicalizes_safe_inputs(
        case in property_support::transforms::valid_transform(),
    ) {
        let transform = RigidTransform::try_new(case.translation, case.rotation_xyzw)
            .expect("safe strategy produces a transform");
        let translation = transform.translation().map(Meters::get);
        let rotation = transform.rotation_xyzw();
        let norm_squared = rotation.into_iter().map(|value| value * value).sum::<f32>();
        let input_norm = case.rotation_xyzw
            .into_iter()
            .map(|value| f64::from(value).powi(2))
            .sum::<f64>()
            .sqrt();
        let alignment = case.rotation_xyzw
            .into_iter()
            .zip(rotation)
            .map(|(input, output)| f64::from(input) / input_norm * f64::from(output))
            .sum::<f64>()
            .abs();
        let expected_translation = case.translation.map(|value| if value == 0.0 { 0.0 } else { value });

        prop_assert!(translation.into_iter().all(f32::is_finite), "case={:?}", case);
        prop_assert_eq!(translation.map(f32::to_bits), expected_translation.map(f32::to_bits),
            "case={:?}, translation={:?}", case, translation);
        prop_assert!(rotation.into_iter().all(f32::is_finite), "case={:?}", case);
        prop_assert!((norm_squared - 1.0).abs() <= 32.0 * f32::EPSILON,
            "case={:?}, rotation={:?}, norm_squared={}", case, rotation, norm_squared);
        prop_assert!((alignment - 1.0).abs() <= 32.0 * f64::from(f32::EPSILON),
            "case={:?}, rotation={:?}, alignment={}", case, rotation, alignment);
        prop_assert!(canonical_sign(rotation), "case={:?}, rotation={:?}", case, rotation);
    }

    #[test]
    fn json_round_trip_preserves_transform_and_canonical_wire(
        case in property_support::transforms::valid_transform(),
    ) {
        let transform = RigidTransform::try_new(case.translation, case.rotation_xyzw).unwrap();
        let json = serde_json::to_value(transform).unwrap();
        let decoded: RigidTransform = serde_json::from_value(json.clone()).unwrap();
        let encoded_again = serde_json::to_value(decoded).unwrap();
        let probe = [3.0, -2.0, 1.0];
        let before = values(transform.transform_point(metres(probe)).unwrap());
        let after = values(decoded.transform_point(metres(probe)).unwrap());

        prop_assert!(points_approximately_equal(before, after),
            "case={:?}, before={:?}, after={:?}, json={}, encoded_again={}",
            case, before, after, json, encoded_again);
        prop_assert!(no_negative_zero(&json["translation"]), "case={:?}, json={}", case, json);
        prop_assert!(no_negative_zero(&json["rotation_xyzw"]), "case={:?}, json={}", case, json);
        prop_assert!(no_negative_zero(&encoded_again["translation"]),
            "case={:?}, json={}", case, encoded_again);
        prop_assert!(no_negative_zero(&encoded_again["rotation_xyzw"]),
            "case={:?}, json={}", case, encoded_again);
        prop_assert!(canonical_sign(decoded.rotation_xyzw()), "case={:?}, json={}", case, json);
    }

    #[test]
    fn inverse_round_trip_recovers_bounded_points(
        case in property_support::transforms::valid_transform(),
        point in property_support::transforms::finite_point(),
    ) {
        let transform = RigidTransform::try_new(case.translation, case.rotation_xyzw).unwrap();
        let transformed = transform.transform_point(metres(point)).expect("bounded finite output");
        let recovered = transform.inverse().expect("bounded finite inverse")
            .transform_point(transformed).expect("bounded finite round trip");
        let recovered = values(recovered);

        prop_assert!(recovered.into_iter().all(f32::is_finite),
            "case={:?}, point={:?}, recovered={:?}", case, point, recovered);
        prop_assert!(points_approximately_equal(recovered, point),
            "case={:?}, point={:?}, recovered={:?}, tolerance={}",
            case, point, recovered, scaled_tolerance(recovered, point));
    }

    #[test]
    fn composition_agrees_with_sequential_point_application(
        left in property_support::transforms::valid_transform(),
        right in property_support::transforms::valid_transform(),
        point in property_support::transforms::finite_point(),
    ) {
        let left_transform = RigidTransform::try_new(left.translation, left.rotation_xyzw).unwrap();
        let right_transform = RigidTransform::try_new(right.translation, right.rotation_xyzw).unwrap();
        let composed = left_transform.compose(right_transform).expect("bounded finite composition");
        let sequential = left_transform.transform_point(
            right_transform.transform_point(metres(point)).expect("finite right output"),
        ).expect("finite sequential output");
        let direct = composed.transform_point(metres(point)).expect("finite composed output");
        let sequential = values(sequential);
        let direct = values(direct);

        prop_assert!(points_approximately_equal(direct, sequential),
            "left={:?}, right={:?}, point={:?}, direct={:?}, sequential={:?}, tolerance={}",
            left, right, point, direct, sequential, scaled_tolerance(direct, sequential));
    }

    #[test]
    fn identity_is_neutral_on_both_sides_and_on_points(
        case in property_support::transforms::valid_transform(),
        point in property_support::transforms::finite_point(),
    ) {
        let transform = RigidTransform::try_new(case.translation, case.rotation_xyzw).unwrap();
        let expected = values(transform.transform_point(metres(point)).unwrap());
        let left = values(RigidTransform::identity().compose(transform).unwrap()
            .transform_point(metres(point)).unwrap());
        let right = values(transform.compose(RigidTransform::identity()).unwrap()
            .transform_point(metres(point)).unwrap());
        let unchanged = values(RigidTransform::identity().transform_point(metres(point)).unwrap());

        prop_assert!(points_approximately_equal(left, expected),
            "case={:?}, point={:?}, left={:?}, expected={:?}", case, point, left, expected);
        prop_assert!(points_approximately_equal(right, expected),
            "case={:?}, point={:?}, right={:?}, expected={:?}", case, point, right, expected);
        prop_assert!(points_approximately_equal(unchanged, point),
            "point={:?}, unchanged={:?}", point, unchanged);
    }
}

#[test]
fn delicate_transform_regressions_remain_canonical() {
    let cases = [
        (
            "near identity",
            [0.0, 0.0, 0.0],
            [f32::EPSILON, 0.0, 0.0, 1.0],
        ),
        ("negative zero", [-0.0, 0.0, -0.0], [-0.0, 0.0, -0.0, 1.0]),
        (
            "negative representative",
            [0.0, 0.0, 0.0],
            [0.0, 0.0, -1.0, -1.0],
        ),
    ];

    for (name, translation, rotation) in cases {
        let transform = RigidTransform::try_new(translation, rotation).expect(name);
        let json = serde_json::to_value(transform).expect(name);
        assert!(no_negative_zero(&json["translation"]), "{name}: {json}");
        assert!(no_negative_zero(&json["rotation_xyzw"]), "{name}: {json}");
        assert!(canonical_sign(transform.rotation_xyzw()), "{name}: {json}");
    }

    let positive = RigidTransform::try_new([0.0; 3], [0.0, 0.0, 1.0, 1.0]).unwrap();
    let negative = RigidTransform::try_new([0.0; 3], [0.0, 0.0, -1.0, -1.0]).unwrap();
    assert_eq!(
        serde_json::to_value(positive).unwrap(),
        serde_json::to_value(negative).unwrap(),
        "sign-equivalent quaternions must select one wire representation",
    );
    assert_eq!(
        serde_json::to_string(
            &RigidTransform::try_new([-0.0; 3], [-0.0, -0.0, -0.0, -1.0]).unwrap()
        )
        .unwrap(),
        r#"{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}"#,
    );
}

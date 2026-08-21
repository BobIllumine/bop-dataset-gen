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

fn translation_values(transform: RigidTransform) -> [f32; 3] {
    transform.translation().map(Meters::get)
}

fn l1_norm(vector: [f32; 3]) -> f32 {
    vector.into_iter().map(f32::abs).sum()
}

fn l2_norm(vector: [f32; 3]) -> f32 {
    vector
        .into_iter()
        .map(|component| component * component)
        .sum::<f32>()
        .sqrt()
}

fn rotated_l1_bound(vector: [f32; 3]) -> f32 {
    // A unit rotation preserves L2, and ||v||_1 <= sqrt(3) * ||v||_2.
    3.0_f32.sqrt() * l2_norm(vector)
}

const TRANSLATION_ADD_STEPS: u16 = 1;
// glam's scalar quaternion-vector formula has two three-term dot products, a
// cross product, and the dependent scalar/vector multiplies and additions.
// There are 22 contributing rounded operations on a component path; 32 also
// covers the equivalent SIMD evaluation order used on supported targets.
const ROTATION_STEPS: u16 = 32;
// normalize_rotation performs four scale divisions, four squares, three
// length additions, one square root, and four final divisions.
const NORMALIZATION_STEPS: u16 = 16;
// One quaternion product component is four products joined by three sums.
const QUATERNION_PRODUCT_STEPS: u16 = 7;
// Include construction normalization(s): conjugation/composition identities
// presume exact unit inputs, while the stored f32 quaternions are rounded.
const INVERSE_COMPARISON_STEPS: u16 = NORMALIZATION_STEPS
    + ROTATION_STEPS
    + TRANSLATION_ADD_STEPS
    + ROTATION_STEPS
    + NORMALIZATION_STEPS
    + ROTATION_STEPS
    + TRANSLATION_ADD_STEPS;
const SEQUENTIAL_APPLICATION_STEPS: u16 = 2 * (ROTATION_STEPS + TRANSLATION_ADD_STEPS);
const COMPOSED_APPLICATION_STEPS: u16 = QUATERNION_PRODUCT_STEPS
    + ROTATION_STEPS
    + TRANSLATION_ADD_STEPS
    + NORMALIZATION_STEPS
    + ROTATION_STEPS
    + TRANSLATION_ADD_STEPS;
const COMPOSITION_COMPARISON_STEPS: u16 =
    2 * NORMALIZATION_STEPS + SEQUENTIAL_APPLICATION_STEPS + COMPOSED_APPLICATION_STEPS;

#[derive(Clone, Copy, Debug)]
struct ErrorBudget {
    operation_scale: f32,
    rounding_steps: u16,
}

impl ErrorBudget {
    fn from_scales<const N: usize>(rounding_steps: u16, scales: [f32; N]) -> Self {
        Self {
            operation_scale: scales.into_iter().fold(1.0_f32, f32::max),
            rounding_steps,
        }
    }

    fn tolerance(self) -> f32 {
        // Standard floating-point forward-error factor gamma_n. Using an
        // absolute operation scale (rather than only the final result) keeps
        // the bound valid when large intermediates cancel near zero.
        let accumulated_epsilon = f32::from(self.rounding_steps) * f32::EPSILON;
        accumulated_epsilon / (1.0 - accumulated_epsilon) * self.operation_scale
    }
}

fn inverse_error_budget(
    point: [f32; 3],
    translation: [f32; 3],
    transformed: [f32; 3],
    inverse_translation: [f32; 3],
    recovered: [f32; 3],
) -> ErrorBudget {
    // Over the main generators, ||p||_2 <= 32*sqrt(3) and ||t||_2 <=
    // 64*sqrt(3). Thus a rotated point has L1 <= 96, a translated point has
    // L1 <= 288, and rotating that intermediate then adding inverse
    // translation has an absolute path bound <= 288 + 192 = 480.
    let forward_rotated = rotated_l1_bound(point);
    let inverse_rotated = rotated_l1_bound(transformed);
    ErrorBudget::from_scales(
        INVERSE_COMPARISON_STEPS,
        [
            l1_norm(point),
            l1_norm(translation),
            forward_rotated,
            forward_rotated + l1_norm(translation),
            l1_norm(transformed),
            l1_norm(inverse_translation),
            inverse_rotated,
            inverse_rotated + l1_norm(inverse_translation),
            l1_norm(recovered),
        ],
    )
}

#[allow(clippy::too_many_arguments)]
fn composition_error_budget(
    left_translation: [f32; 3],
    right_translation: [f32; 3],
    point: [f32; 3],
    right_output: [f32; 3],
    composed_translation: [f32; 3],
    direct: [f32; 3],
    sequential: [f32; 3],
) -> ErrorBudget {
    // The same generator limits give a right output L1 bound of 288. Its L2
    // path bound is 96*sqrt(3), so the second rotation is L1 <= 288 and the
    // final translation makes 480. The composed translation is L1 <= 384;
    // rotating p and adding it also makes 480. This is the true worst-case
    // operation_scale passed to gamma_n, even if the compared outputs cancel.
    let right_rotated = rotated_l1_bound(point);
    let sequential_rotated = rotated_l1_bound(right_output);
    let composed_translation_rotated = rotated_l1_bound(right_translation);
    let direct_rotated = rotated_l1_bound(point);
    ErrorBudget::from_scales(
        COMPOSITION_COMPARISON_STEPS,
        [
            l1_norm(left_translation),
            l1_norm(right_translation),
            l1_norm(point),
            right_rotated,
            right_rotated + l1_norm(right_translation),
            l1_norm(right_output),
            sequential_rotated,
            sequential_rotated + l1_norm(left_translation),
            composed_translation_rotated,
            composed_translation_rotated + l1_norm(left_translation),
            l1_norm(composed_translation),
            direct_rotated,
            direct_rotated + l1_norm(composed_translation),
            l1_norm(direct),
            l1_norm(sequential),
        ],
    )
}

fn points_approximately_equal(left: [f32; 3], right: [f32; 3], budget: ErrorBudget) -> bool {
    let tolerance = budget.tolerance();
    left.into_iter()
        .zip(right)
        .all(|(left, right)| (left - right).abs() <= tolerance)
}

fn quaternion_l1(rotation: [f32; 4]) -> f32 {
    rotation.into_iter().map(f32::abs).sum()
}

fn quaternion_component_error(left: [f32; 4], right: [f32; 4]) -> f32 {
    let same_sign = left
        .into_iter()
        .zip(right)
        .map(|(left, right)| (left - right).abs())
        .fold(0.0_f32, f32::max);
    let opposite_sign = left
        .into_iter()
        .zip(right)
        .map(|(left, right)| (left + right).abs())
        .fold(0.0_f32, f32::max);
    same_sign.min(opposite_sign)
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
        let encoded = serde_json::to_string(&transform).unwrap();
        let decoded: RigidTransform = serde_json::from_str(&encoded).unwrap();
        let json = serde_json::to_value(transform).unwrap();
        let reencoded = serde_json::to_string(&decoded).unwrap();
        let encoded_again: serde_json::Value = serde_json::from_str(&reencoded).unwrap();
        let transform_translation = translation_values(transform);
        let decoded_translation = translation_values(decoded);
        let transform_rotation = transform.rotation_xyzw();
        let decoded_rotation = decoded.rotation_xyzw();
        let rotation_budget = ErrorBudget::from_scales(
            NORMALIZATION_STEPS,
            [
                quaternion_l1(transform_rotation),
                quaternion_l1(decoded_rotation),
            ],
        );
        let alignment = transform_rotation
            .into_iter()
            .zip(decoded_rotation)
            .map(|(left, right)| f64::from(left) * f64::from(right))
            .sum::<f64>()
            .abs();

        prop_assert_eq!(decoded_translation.map(f32::to_bits),
            transform_translation.map(f32::to_bits),
            "case={:?}, encoded={}, reencoded={}", case, encoded, reencoded);
        prop_assert!(quaternion_component_error(transform_rotation, decoded_rotation)
                <= rotation_budget.tolerance(),
            "case={:?}, transform_rotation={:?}, decoded_rotation={:?}, budget={:?}, tolerance={}",
            case, transform_rotation, decoded_rotation, rotation_budget, rotation_budget.tolerance());
        prop_assert!((alignment - 1.0).abs()
                <= 4.0 * f64::from(rotation_budget.tolerance()),
            "case={:?}, transform_rotation={:?}, decoded_rotation={:?}, alignment={}, tolerance={}",
            case, transform_rotation, decoded_rotation, alignment, rotation_budget.tolerance());
        prop_assert!(no_negative_zero(&json["translation"]), "case={:?}, json={}", case, json);
        prop_assert!(no_negative_zero(&json["rotation_xyzw"]), "case={:?}, json={}", case, json);
        prop_assert!(no_negative_zero(&encoded_again["translation"]),
            "case={:?}, json={}", case, encoded_again);
        prop_assert!(no_negative_zero(&encoded_again["rotation_xyzw"]),
            "case={:?}, json={}", case, encoded_again);
        prop_assert!(canonical_sign(transform_rotation), "case={:?}, json={}", case, json);
        prop_assert!(canonical_sign(decoded_rotation),
            "case={:?}, reencoded={}", case, encoded_again);
    }

    #[test]
    fn inverse_round_trip_recovers_bounded_points(
        case in property_support::transforms::valid_transform(),
        point in property_support::transforms::finite_point(),
    ) {
        let transform = RigidTransform::try_new(case.translation, case.rotation_xyzw).unwrap();
        let transformed = transform.transform_point(metres(point)).expect("bounded finite output");
        let inverse = transform.inverse().expect("bounded finite inverse");
        let recovered = inverse.transform_point(transformed).expect("bounded finite round trip");
        let transformed = values(transformed);
        let recovered = values(recovered);
        let budget = inverse_error_budget(
            point,
            translation_values(transform),
            transformed,
            translation_values(inverse),
            recovered,
        );

        prop_assert!(recovered.into_iter().all(f32::is_finite),
            "case={:?}, point={:?}, recovered={:?}", case, point, recovered);
        prop_assert!(points_approximately_equal(recovered, point, budget),
            "case={:?}, point={:?}, transformed={:?}, recovered={:?}, budget={:?}, tolerance={}",
            case, point, transformed, recovered, budget, budget.tolerance());
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
        let right_output = right_transform.transform_point(metres(point)).expect("finite right output");
        let sequential = left_transform.transform_point(right_output).expect("finite sequential output");
        let direct = composed.transform_point(metres(point)).expect("finite composed output");
        let right_output = values(right_output);
        let sequential = values(sequential);
        let direct = values(direct);
        let budget = composition_error_budget(
            translation_values(left_transform),
            translation_values(right_transform),
            point,
            right_output,
            translation_values(composed),
            direct,
            sequential,
        );

        prop_assert!(points_approximately_equal(direct, sequential, budget),
            "left={:?}, right={:?}, point={:?}, right_output={:?}, direct={:?}, sequential={:?}, budget={:?}, tolerance={}",
            left, right, point, right_output, direct, sequential, budget, budget.tolerance());
    }

    #[test]
    fn identity_is_neutral_on_both_sides_and_on_points(
        case in property_support::transforms::valid_transform(),
        point in property_support::transforms::finite_point(),
    ) {
        let transform = RigidTransform::try_new(case.translation, case.rotation_xyzw).unwrap();
        let expected = values(transform.transform_point(metres(point)).unwrap());
        let left_composed = RigidTransform::identity().compose(transform).unwrap();
        let right_composed = transform.compose(RigidTransform::identity()).unwrap();
        let left = values(left_composed.transform_point(metres(point)).unwrap());
        let right = values(right_composed.transform_point(metres(point)).unwrap());
        let unchanged = values(RigidTransform::identity().transform_point(metres(point)).unwrap());
        let canonical_point = point.map(|value| if value == 0.0 { 0.0 } else { value });
        let left_budget = composition_error_budget(
            [0.0; 3],
            translation_values(transform),
            point,
            expected,
            translation_values(left_composed),
            left,
            expected,
        );
        let right_budget = composition_error_budget(
            translation_values(transform),
            [0.0; 3],
            point,
            canonical_point,
            translation_values(right_composed),
            right,
            expected,
        );

        prop_assert!(points_approximately_equal(left, expected, left_budget),
            "case={:?}, point={:?}, left={:?}, expected={:?}", case, point, left, expected);
        prop_assert!(points_approximately_equal(right, expected, right_budget),
            "case={:?}, point={:?}, right={:?}, expected={:?}", case, point, right, expected);
        prop_assert_eq!(unchanged.map(f32::to_bits), canonical_point.map(f32::to_bits),
            "point={:?}, unchanged={:?}", point, unchanged);
    }

    #[test]
    fn cancellation_heavy_composition_respects_intermediate_scale_budget(
        case in property_support::transforms::cancellation_composition(),
    ) {
        let right = RigidTransform::try_new(case.right_translation, case.right_rotation_xyzw)
            .expect("bounded right transform");
        let right_output = right.transform_point(metres(case.point)).expect("bounded right output");
        let left_rotation_only = RigidTransform::try_new([0.0; 3], case.left_rotation_xyzw)
            .expect("bounded left rotation");
        let left_rotated = values(left_rotation_only.transform_point(right_output)
            .expect("bounded rotated intermediate"));
        let left_translation = left_rotated.map(|value| -value);
        let left = RigidTransform::try_new(left_translation, case.left_rotation_xyzw)
            .expect("cancellation translation remains finite");
        let composed = left.compose(right).expect("bounded composition");
        let sequential = values(left.transform_point(right_output).expect("bounded sequential output"));
        let direct = values(composed.transform_point(metres(case.point))
            .expect("bounded direct output"));
        let right_output = values(right_output);
        let budget = composition_error_budget(
            left_translation,
            case.right_translation,
            case.point,
            right_output,
            translation_values(composed),
            direct,
            sequential,
        );

        prop_assert!(left_translation.into_iter().all(|value| value.abs() <= 64.0),
            "case={:?}, left_translation={:?}", case, left_translation);
        prop_assert_eq!(sequential.map(f32::to_bits), [0.0_f32.to_bits(); 3],
            "case={:?}, left_translation={:?}, sequential={:?}", case, left_translation, sequential);
        prop_assert!(points_approximately_equal(direct, sequential, budget),
            "case={:?}, left_translation={:?}, right_output={:?}, direct={:?}, sequential={:?}, budget={:?}, tolerance={}",
            case, left_translation, right_output, direct, sequential, budget, budget.tolerance());
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

#[test]
fn json_round_trip_allows_one_ulp_of_canonical_renormalization() {
    let transform =
        RigidTransform::try_new([-0.0; 3], [-0.0, -2.601_562_5, 1.128_906_3, 0.402_343_75])
            .unwrap();
    let encoded = serde_json::to_string(&transform).unwrap();
    let decoded: RigidTransform = serde_json::from_str(&encoded).unwrap();
    let reencoded = serde_json::to_string(&decoded).unwrap();
    let before = transform.rotation_xyzw();
    let after = decoded.rotation_xyzw();
    let budget = ErrorBudget::from_scales(
        NORMALIZATION_STEPS,
        [quaternion_l1(before), quaternion_l1(after)],
    );

    assert_ne!(
        encoded, reencoded,
        "the fixture must exercise re-normalization"
    );
    assert_eq!(before[2].to_bits().abs_diff(after[2].to_bits()), 1);
    assert!(quaternion_component_error(before, after) <= budget.tolerance());
    assert!(canonical_sign(before));
    assert!(canonical_sign(after));
    assert_eq!(
        translation_values(transform).map(f32::to_bits),
        translation_values(decoded).map(f32::to_bits),
    );
}

#[test]
fn cancellation_regression_uses_intermediate_not_final_output_scale() {
    let rotation = [0.1, -0.7, 0.3, 0.2];
    let point = [32.0, -32.0, 31.875];
    let right = RigidTransform::try_new([0.0; 3], rotation).unwrap();
    let right_output = right.transform_point(metres(point)).unwrap();
    let left_rotation_only = RigidTransform::try_new([0.0; 3], rotation).unwrap();
    let left_translation =
        values(left_rotation_only.transform_point(right_output).unwrap()).map(|value| -value);
    let left = RigidTransform::try_new(left_translation, rotation).unwrap();
    let composed = left.compose(right).unwrap();
    let sequential = values(left.transform_point(right_output).unwrap());
    let direct = values(composed.transform_point(metres(point)).unwrap());
    let right_output = values(right_output);
    let budget = composition_error_budget(
        left_translation,
        [0.0; 3],
        point,
        right_output,
        translation_values(composed),
        direct,
        sequential,
    );
    let error = direct
        .into_iter()
        .zip(sequential)
        .map(|(direct, sequential)| (direct - sequential).abs())
        .fold(0.0_f32, f32::max);
    let old_final_only_tolerance = 128.0
        * f32::EPSILON
        * direct
            .into_iter()
            .chain(sequential)
            .map(f32::abs)
            .fold(1.0_f32, f32::max);

    assert!(
        error > old_final_only_tolerance,
        "fixture no longer distinguishes final-only scaling: error={error}, old={old_final_only_tolerance}"
    );
    assert!(budget.operation_scale > 1.0);
    assert!(
        budget.operation_scale <= 480.0,
        "generated-range proof bounds the operation scale at 480: {budget:?}"
    );
    assert!(
        points_approximately_equal(direct, sequential, budget),
        "direct={direct:?}, sequential={sequential:?}, budget={budget:?}"
    );
}

//! Numerically safe, shrinkable rigid-transform cases.

use proptest::prelude::*;

#[derive(Clone, Copy, Debug)]
pub struct TransformCase {
    pub translation: [f32; 3],
    pub rotation_xyzw: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct CancellationCompositionCase {
    pub left_rotation_xyzw: [f32; 4],
    pub right_rotation_xyzw: [f32; 4],
    pub right_translation: [f32; 3],
    pub point: [f32; 3],
}

fn signed_component(limit: i16, divisor: f32) -> BoxedStrategy<f32> {
    prop_oneof![
        Just(-0.0_f32),
        (-limit..=limit).prop_map(move |value| f32::from(value) / divisor),
    ]
    .boxed()
}

pub fn finite_translation() -> BoxedStrategy<[f32; 3]> {
    (
        signed_component(512, 8.0),
        signed_component(512, 8.0),
        signed_component(512, 8.0),
    )
        .prop_map(|(x, y, z)| [x, y, z])
        .boxed()
}

pub fn finite_point() -> BoxedStrategy<[f32; 3]> {
    (
        signed_component(256, 8.0),
        signed_component(256, 8.0),
        signed_component(256, 8.0),
    )
        .prop_map(|(x, y, z)| [x, y, z])
        .boxed()
}

fn cancellation_translation() -> BoxedStrategy<[f32; 3]> {
    (
        signed_component(64, 8.0),
        signed_component(64, 8.0),
        signed_component(64, 8.0),
    )
        .prop_map(|(x, y, z)| [x, y, z])
        .boxed()
}

fn cancellation_point() -> BoxedStrategy<[f32; 3]> {
    (
        signed_component(128, 8.0),
        signed_component(128, 8.0),
        signed_component(128, 8.0),
    )
        .prop_map(|(x, y, z)| [x, y, z])
        .boxed()
}

pub fn nondegenerate_quaternion() -> BoxedStrategy<[f32; 4]> {
    (
        (
            signed_component(1_024, 256.0),
            signed_component(1_024, 256.0),
            signed_component(1_024, 256.0),
            signed_component(1_024, 256.0),
        ),
        0_usize..4,
        1_i16..=1_024,
        any::<bool>(),
    )
        .prop_map(|((x, y, z, w), pivot, magnitude, negative)| {
            let mut quaternion = [x, y, z, w];
            quaternion[pivot] = f32::from(magnitude) / 256.0 * if negative { -1.0 } else { 1.0 };
            quaternion
        })
        .boxed()
}

pub fn valid_transform() -> BoxedStrategy<TransformCase> {
    (finite_translation(), nondegenerate_quaternion())
        .prop_map(|(translation, rotation_xyzw)| TransformCase {
            translation,
            rotation_xyzw,
        })
        .boxed()
}

pub fn cancellation_composition() -> BoxedStrategy<CancellationCompositionCase> {
    (
        nondegenerate_quaternion(),
        nondegenerate_quaternion(),
        cancellation_translation(),
        cancellation_point(),
    )
        .prop_map(
            |(left_rotation_xyzw, right_rotation_xyzw, right_translation, point)| {
                CancellationCompositionCase {
                    left_rotation_xyzw,
                    right_rotation_xyzw,
                    right_translation,
                    point,
                }
            },
        )
        .boxed()
}

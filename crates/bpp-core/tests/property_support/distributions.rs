//! Shrinkable value models for distribution properties.

use proptest::{collection::vec, prelude::*};

#[derive(Clone, Debug)]
pub enum DistributionCase<T> {
    Fixed(T),
    Uniform { minimum: T, maximum: T },
    Choice(Vec<(T, f64)>),
}

#[derive(Clone, Debug)]
pub enum InvalidU32Distribution {
    InvertedUniform { minimum: u32, maximum: u32 },
    EmptyChoice,
    AllZeroChoice(Vec<u32>),
}

#[derive(Clone, Debug)]
pub enum InvalidF32Distribution {
    NonFiniteFixed(f32),
    NonFiniteMinimum { minimum: f32, maximum: f32 },
    NonFiniteMaximum { minimum: f32, maximum: f32 },
    InvertedUniform { minimum: f32, maximum: f32 },
    EmptyChoice,
    AllZeroChoice(Vec<f32>),
    NonFiniteChoice(f32),
}

#[derive(Clone, Debug)]
pub enum InvalidAssetDistribution {
    Uniform { minimum: String, maximum: String },
    EmptyChoice,
    AllZeroChoice(Vec<String>),
    BlankFixed(String),
}

pub fn finite_f32() -> BoxedStrategy<f32> {
    prop_oneof![
        Just(-0.0_f32),
        Just(0.0_f32),
        (-8_192_i16..=8_192).prop_map(|value| f32::from(value) / 16.0),
    ]
    .boxed()
}

pub fn finite_nonnegative_weight() -> BoxedStrategy<f64> {
    (0_u16..=512)
        .prop_map(|value| f64::from(value) / 16.0)
        .boxed()
}

pub fn finite_positive_weight() -> BoxedStrategy<f64> {
    (1_u16..=512)
        .prop_map(|value| f64::from(value) / 16.0)
        .boxed()
}

pub fn invalid_weight() -> BoxedStrategy<f64> {
    prop_oneof![
        (-512_i16..=-1).prop_map(|value| f64::from(value) / 16.0),
        prop::sample::select(vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY]),
    ]
    .boxed()
}

pub fn asset_id_string() -> BoxedStrategy<String> {
    (0_u16..=1_024)
        .prop_map(|index| format!("asset-{index:04}"))
        .boxed()
}

fn ordered_u32_bounds() -> BoxedStrategy<(u32, u32)> {
    prop_oneof![
        (0_u32..=65_536).prop_map(|value| (value, value)),
        (0_u32..=65_536, 0_u32..=65_536)
            .prop_map(|(left, right)| (left.min(right), left.max(right))),
    ]
    .boxed()
}

fn ordered_f32_bounds() -> BoxedStrategy<(f32, f32)> {
    prop_oneof![
        finite_f32().prop_map(|value| (value, value)),
        (finite_f32(), finite_f32()).prop_map(|(left, right)| {
            if left <= right {
                (left, right)
            } else {
                (right, left)
            }
        }),
    ]
    .boxed()
}

fn valid_choice<T>(value: BoxedStrategy<T>) -> BoxedStrategy<Vec<(T, f64)>>
where
    T: Clone + std::fmt::Debug + 'static,
{
    (
        (value.clone(), finite_positive_weight()),
        vec((value, finite_nonnegative_weight()), 0..=3),
    )
        .prop_map(|(first, rest)| std::iter::once(first).chain(rest).collect())
        .boxed()
}

pub fn valid_u32_distribution() -> BoxedStrategy<DistributionCase<u32>> {
    let values = (0_u32..=65_536).boxed();
    prop_oneof![
        values.clone().prop_map(DistributionCase::Fixed),
        ordered_u32_bounds()
            .prop_map(|(minimum, maximum)| DistributionCase::Uniform { minimum, maximum }),
        valid_choice(values).prop_map(DistributionCase::Choice),
    ]
    .boxed()
}

pub fn valid_f32_distribution() -> BoxedStrategy<DistributionCase<f32>> {
    let values = finite_f32();
    prop_oneof![
        values.clone().prop_map(DistributionCase::Fixed),
        ordered_f32_bounds()
            .prop_map(|(minimum, maximum)| DistributionCase::Uniform { minimum, maximum }),
        valid_choice(values).prop_map(DistributionCase::Choice),
    ]
    .boxed()
}

pub fn valid_asset_distribution() -> BoxedStrategy<DistributionCase<String>> {
    let values = asset_id_string();
    prop_oneof![
        values.clone().prop_map(DistributionCase::Fixed),
        valid_choice(values).prop_map(DistributionCase::Choice),
    ]
    .boxed()
}

pub fn invalid_u32_distribution() -> BoxedStrategy<InvalidU32Distribution> {
    prop_oneof![
        (0_u32..65_536, 1_u32..=1_024).prop_map(|(maximum, delta)| {
            InvalidU32Distribution::InvertedUniform {
                minimum: maximum.saturating_add(delta),
                maximum,
            }
        }),
        Just(InvalidU32Distribution::EmptyChoice),
        vec(0_u32..=65_536, 1..=4).prop_map(InvalidU32Distribution::AllZeroChoice),
    ]
    .boxed()
}

pub fn invalid_f32_distribution() -> BoxedStrategy<InvalidF32Distribution> {
    let nonfinite =
        || prop::sample::select(vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY]).boxed();
    prop_oneof![
        nonfinite().prop_map(InvalidF32Distribution::NonFiniteFixed),
        (nonfinite(), finite_f32()).prop_map(|(minimum, maximum)| {
            InvalidF32Distribution::NonFiniteMinimum { minimum, maximum }
        }),
        (finite_f32(), nonfinite()).prop_map(|(minimum, maximum)| {
            InvalidF32Distribution::NonFiniteMaximum { minimum, maximum }
        }),
        (-8_192_i16..8_192, 1_i16..=1_024).prop_map(|(base, delta)| {
            InvalidF32Distribution::InvertedUniform {
                minimum: f32::from(base + delta) / 16.0,
                maximum: f32::from(base) / 16.0,
            }
        }),
        Just(InvalidF32Distribution::EmptyChoice),
        vec(finite_f32(), 1..=4).prop_map(InvalidF32Distribution::AllZeroChoice),
        nonfinite().prop_map(InvalidF32Distribution::NonFiniteChoice),
    ]
    .boxed()
}

pub fn invalid_asset_distribution() -> BoxedStrategy<InvalidAssetDistribution> {
    prop_oneof![
        (asset_id_string(), asset_id_string()).prop_map(|(minimum, maximum)| {
            InvalidAssetDistribution::Uniform { minimum, maximum }
        }),
        Just(InvalidAssetDistribution::EmptyChoice),
        vec(asset_id_string(), 1..=4).prop_map(InvalidAssetDistribution::AllZeroChoice),
        prop::sample::select(vec![String::new(), String::from(" "), String::from("\t")])
            .prop_map(InvalidAssetDistribution::BlankFixed),
    ]
    .boxed()
}

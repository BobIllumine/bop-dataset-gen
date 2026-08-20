//! Bounded generative tests for checked distribution contracts.

mod property_support;

use bpp_core::{
    AssetId, Distribution, DistributionView, InvalidDistributionError, InvalidWeightedValueError,
    UniformBound, UniformSemantics, WeightedValue,
};
use property_support::distributions::{
    DistributionCase, InvalidAssetDistribution, InvalidF32Distribution, InvalidU32Distribution,
};
use proptest::prelude::*;

fn canonical_f32(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

fn canonical_f64(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

fn u32_case(case: &DistributionCase<u32>) -> Distribution<u32> {
    match case {
        DistributionCase::Fixed(value) => Distribution::<u32>::fixed(*value),
        DistributionCase::Uniform { minimum, maximum } => {
            Distribution::<u32>::try_uniform(*minimum, *maximum)
                .expect("strategy orders integer bounds")
        }
        DistributionCase::Choice(values) => Distribution::<u32>::try_choice(
            values
                .iter()
                .map(|(value, weight)| WeightedValue::try_new(*value, *weight).unwrap())
                .collect(),
        )
        .expect("strategy keeps a positive choice weight"),
    }
}

fn f32_case(case: &DistributionCase<f32>) -> Distribution<f32> {
    match case {
        DistributionCase::Fixed(value) => {
            Distribution::<f32>::try_fixed(*value).expect("strategy generates finite values")
        }
        DistributionCase::Uniform { minimum, maximum } => {
            Distribution::<f32>::try_uniform(*minimum, *maximum)
                .expect("strategy orders finite bounds")
        }
        DistributionCase::Choice(values) => Distribution::<f32>::try_choice(
            values
                .iter()
                .map(|(value, weight)| WeightedValue::try_new(*value, *weight).unwrap())
                .collect(),
        )
        .expect("strategy keeps finite values and a positive weight"),
    }
}

fn asset_case(case: &DistributionCase<String>) -> Distribution<AssetId> {
    match case {
        DistributionCase::Fixed(value) => Distribution::<AssetId>::fixed(
            AssetId::new(value.clone()).expect("nonblank strategy ID"),
        ),
        DistributionCase::Uniform { .. } => unreachable!("asset strategy has no uniform branch"),
        DistributionCase::Choice(values) => Distribution::<AssetId>::try_choice(
            values
                .iter()
                .map(|(value, weight)| {
                    WeightedValue::try_new(
                        AssetId::new(value.clone()).expect("nonblank strategy ID"),
                        *weight,
                    )
                    .unwrap()
                })
                .collect(),
        )
        .expect("strategy keeps a positive choice weight"),
    }
}

fn wire_json<T: serde::Serialize>(case: &DistributionCase<T>) -> serde_json::Value {
    match case {
        DistributionCase::Fixed(value) => serde_json::json!({"kind": "fixed", "value": value}),
        DistributionCase::Uniform { minimum, maximum } => {
            serde_json::json!({"kind": "uniform", "min": minimum, "max": maximum})
        }
        DistributionCase::Choice(values) => {
            let values = values
                .iter()
                .map(|(value, weight)| serde_json::json!({"value": value, "weight": weight}))
                .collect::<Vec<_>>();
            serde_json::json!({"kind": "choice", "values": values})
        }
    }
}

fn canonical_f32_wire_json(case: &DistributionCase<f32>) -> serde_json::Value {
    match case {
        DistributionCase::Fixed(value) => {
            serde_json::json!({"kind": "fixed", "value": canonical_f32(*value)})
        }
        DistributionCase::Uniform { minimum, maximum } => serde_json::json!({
            "kind": "uniform",
            "min": canonical_f32(*minimum),
            "max": canonical_f32(*maximum),
        }),
        DistributionCase::Choice(values) => {
            let values = values
                .iter()
                .map(|(value, weight)| {
                    serde_json::json!({
                        "value": canonical_f32(*value),
                        "weight": canonical_f64(*weight),
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({"kind": "choice", "values": values})
        }
    }
}

fn assert_u32_invariants(case: &DistributionCase<u32>, value: &Distribution<u32>) {
    match (case, value.view()) {
        (DistributionCase::Fixed(expected), DistributionView::Fixed { value }) => {
            assert_eq!(value, expected);
        }
        (
            DistributionCase::Uniform { minimum, maximum },
            DistributionView::Uniform {
                min,
                max,
                semantics,
            },
        ) => {
            assert_eq!((*min, *max), (*minimum, *maximum));
            assert_eq!(semantics, UniformSemantics::Inclusive);
        }
        (DistributionCase::Choice(expected), DistributionView::Choice { values }) => {
            assert_eq!(values.len(), expected.len());
            for (actual, (value, weight)) in values.iter().zip(expected) {
                assert_eq!(actual.value(), value);
                assert_eq!(actual.weight().to_bits(), canonical_f64(*weight).to_bits());
            }
            assert!(values.iter().any(|entry| entry.weight() > 0.0));
            assert!(
                values
                    .iter()
                    .all(|entry| entry.weight().is_finite() && entry.weight() >= 0.0)
            );
        }
        _ => panic!("constructor changed distribution variant for {case:?}"),
    }
}

fn assert_f32_invariants(case: &DistributionCase<f32>, value: &Distribution<f32>) {
    match (case, value.view()) {
        (DistributionCase::Fixed(expected), DistributionView::Fixed { value }) => {
            assert_eq!(value.to_bits(), canonical_f32(*expected).to_bits());
        }
        (
            DistributionCase::Uniform { minimum, maximum },
            DistributionView::Uniform {
                min,
                max,
                semantics,
            },
        ) => {
            assert_eq!(min.to_bits(), canonical_f32(*minimum).to_bits());
            assert_eq!(max.to_bits(), canonical_f32(*maximum).to_bits());
            let expected = if canonical_f32(*minimum).to_bits() == canonical_f32(*maximum).to_bits()
            {
                UniformSemantics::Fixed
            } else {
                UniformSemantics::ClosedOpen
            };
            assert_eq!(semantics, expected);
        }
        (DistributionCase::Choice(expected), DistributionView::Choice { values }) => {
            assert_eq!(values.len(), expected.len());
            for (actual, (value, weight)) in values.iter().zip(expected) {
                assert_eq!(actual.value().to_bits(), canonical_f32(*value).to_bits());
                assert_eq!(actual.weight().to_bits(), canonical_f64(*weight).to_bits());
            }
            assert!(values.iter().any(|entry| entry.weight() > 0.0));
        }
        _ => panic!("constructor changed distribution variant for {case:?}"),
    }
}

proptest! {
    #![proptest_config(property_support::suite_config(64))]

    #[test]
    fn valid_integer_distributions_preserve_constructor_and_json_contracts(
        case in property_support::distributions::valid_u32_distribution(),
    ) {
        let constructed = u32_case(&case);
        let wire = wire_json(&case);
        let decoded: Distribution<u32> = serde_json::from_value(wire.clone())
            .expect("valid generated integer wire");

        prop_assert_eq!(&decoded, &constructed, "case={:?}", case);
        prop_assert_eq!(serde_json::to_value(&constructed).unwrap(), wire, "case={:?}", case);
        assert_u32_invariants(&case, &constructed);
    }

    #[test]
    fn valid_float_distributions_preserve_constructor_and_json_contracts(
        case in property_support::distributions::valid_f32_distribution(),
    ) {
        let constructed = f32_case(&case);
        let raw_wire = wire_json(&case);
        let canonical_wire = canonical_f32_wire_json(&case);
        let decoded: Distribution<f32> = serde_json::from_value(raw_wire.clone())
            .expect("valid generated float wire");

        prop_assert_eq!(&decoded, &constructed, "case={:?}, raw_wire={}", case, raw_wire);
        prop_assert_eq!(serde_json::to_value(&constructed).unwrap(), canonical_wire.clone(), "case={:?}", case);
        prop_assert_eq!(serde_json::to_value(&decoded).unwrap(), canonical_wire, "case={:?}", case);
        assert_f32_invariants(&case, &constructed);
    }

    #[test]
    fn valid_asset_distributions_have_no_uniform_form_and_round_trip_json(
        case in property_support::distributions::valid_asset_distribution(),
    ) {
        let constructed = asset_case(&case);
        let wire = wire_json(&case);
        let decoded: Distribution<AssetId> = serde_json::from_value(wire.clone())
            .expect("valid generated asset wire");

        prop_assert_eq!(&decoded, &constructed, "case={:?}", case);
        prop_assert_eq!(serde_json::to_value(&constructed).unwrap(), wire, "case={:?}", case);
        assert!(!matches!(constructed.view(), DistributionView::Uniform { .. }));
    }

    #[test]
    fn weighted_values_accept_exactly_finite_nonnegative_weights(
        value in any::<u32>(),
        weight in property_support::distributions::finite_nonnegative_weight(),
    ) {
        let weighted = WeightedValue::try_new(value, weight).expect("valid generated weight");
        let wire = serde_json::to_value(&weighted).unwrap();
        let decoded: WeightedValue<u32> = serde_json::from_value(wire.clone()).unwrap();

        prop_assert_eq!(weighted.value(), &value);
        prop_assert_eq!(weighted.weight().to_bits(), canonical_f64(weight).to_bits());
        prop_assert_eq!(decoded, weighted, "value={}, weight={}, wire={}", value, weight, wire);
    }

    #[test]
    fn weighted_values_reject_negative_and_nonfinite_weights(
        weight in property_support::distributions::invalid_weight(),
    ) {
        let error = WeightedValue::try_new(7_u32, weight).expect_err("invalid generated weight");
        if weight.is_finite() {
            prop_assert!(matches!(error, InvalidWeightedValueError::NegativeWeight { weight: actual }
                if actual.to_bits() == weight.to_bits()), "weight={weight}");
            let wire = serde_json::json!({"value": 7, "weight": weight});
            prop_assert!(serde_json::from_value::<WeightedValue<u32>>(wire.clone()).is_err(),
                "weight={}, wire={}", weight, wire);
        } else {
            prop_assert!(matches!(error, InvalidWeightedValueError::NonFiniteWeight { weight: actual }
                if actual.to_bits() == weight.to_bits()), "weight={weight:?}");
            let wire = format!("value = 7\nweight = {}\n", toml_weight(weight));
            prop_assert!(toml::from_str::<WeightedValue<u32>>(&wire).is_err(),
                "weight={:?}, wire={}", weight, wire);
        }
    }

    #[test]
    fn invalid_integer_distributions_are_rejected_by_constructor_and_serde(
        case in property_support::distributions::invalid_u32_distribution(),
    ) {
        match case {
            InvalidU32Distribution::InvertedUniform { minimum, maximum } => {
                prop_assert_eq!(
                    Distribution::<u32>::try_uniform(minimum, maximum),
                    Err(InvalidDistributionError::InvertedUniformBounds),
                );
                let wire = serde_json::json!({"kind": "uniform", "min": minimum, "max": maximum});
                prop_assert!(serde_json::from_value::<Distribution<u32>>(wire.clone()).is_err(), "wire={wire}");
            }
            InvalidU32Distribution::EmptyChoice => {
                prop_assert_eq!(Distribution::<u32>::try_choice(Vec::new()), Err(InvalidDistributionError::EmptyChoice));
                let wire = serde_json::json!({"kind": "choice", "values": []});
                prop_assert!(serde_json::from_value::<Distribution<u32>>(wire.clone()).is_err(), "wire={wire}");
            }
            InvalidU32Distribution::AllZeroChoice(values) => {
                let choices = values.iter().map(|value| WeightedValue::try_new(*value, 0.0).unwrap()).collect();
                prop_assert_eq!(Distribution::<u32>::try_choice(choices), Err(InvalidDistributionError::AllZeroChoice));
                let values = values.into_iter().map(|value| serde_json::json!({"value": value, "weight": 0.0})).collect::<Vec<_>>();
                let wire = serde_json::json!({"kind": "choice", "values": values});
                prop_assert!(serde_json::from_value::<Distribution<u32>>(wire.clone()).is_err(), "wire={wire}");
            }
        }
    }

    #[test]
    fn invalid_float_distributions_are_rejected_at_the_same_boundary(
        case in property_support::distributions::invalid_f32_distribution(),
    ) {
        match case {
            InvalidF32Distribution::NonFiniteFixed(value) => {
                let error = Distribution::<f32>::try_fixed(value).unwrap_err();
                assert!(matches!(error, InvalidDistributionError::NonFiniteValue { value: actual }
                    if actual.to_bits() == value.to_bits()), "value={value:?}");
                let wire = format!("kind = \"fixed\"\nvalue = {}\n", toml_float(value));
                prop_assert!(toml::from_str::<Distribution<f32>>(&wire).is_err(), "wire={wire}");
            }
            InvalidF32Distribution::NonFiniteMinimum { minimum, maximum } => {
                let error = Distribution::<f32>::try_uniform(minimum, maximum).unwrap_err();
                assert!(matches!(error, InvalidDistributionError::NonFiniteUniformBound {
                    bound: UniformBound::Minimum, value,
                } if value.to_bits() == minimum.to_bits()), "minimum={minimum:?}");
                let wire = format!("kind = \"uniform\"\nmin = {}\nmax = {maximum}\n", toml_float(minimum));
                prop_assert!(toml::from_str::<Distribution<f32>>(&wire).is_err(), "wire={wire}");
            }
            InvalidF32Distribution::NonFiniteMaximum { minimum, maximum } => {
                let error = Distribution::<f32>::try_uniform(minimum, maximum).unwrap_err();
                assert!(matches!(error, InvalidDistributionError::NonFiniteUniformBound {
                    bound: UniformBound::Maximum, value,
                } if value.to_bits() == maximum.to_bits()), "maximum={maximum:?}");
                let wire = format!("kind = \"uniform\"\nmin = {minimum}\nmax = {}\n", toml_float(maximum));
                prop_assert!(toml::from_str::<Distribution<f32>>(&wire).is_err(), "wire={wire}");
            }
            InvalidF32Distribution::InvertedUniform { minimum, maximum } => {
                prop_assert_eq!(Distribution::<f32>::try_uniform(minimum, maximum), Err(InvalidDistributionError::InvertedUniformBounds));
                let wire = format!("kind = \"uniform\"\nmin = {minimum}\nmax = {maximum}\n");
                prop_assert!(toml::from_str::<Distribution<f32>>(&wire).is_err(), "wire={wire}");
            }
            InvalidF32Distribution::EmptyChoice => {
                prop_assert_eq!(Distribution::<f32>::try_choice(Vec::new()), Err(InvalidDistributionError::EmptyChoice));
                prop_assert!(toml::from_str::<Distribution<f32>>("kind = \"choice\"\nvalues = []\n").is_err());
            }
            InvalidF32Distribution::AllZeroChoice(values) => {
                let choices = values.iter().map(|value| WeightedValue::try_new(*value, 0.0).unwrap()).collect();
                prop_assert_eq!(Distribution::<f32>::try_choice(choices), Err(InvalidDistributionError::AllZeroChoice));
                let values = values.iter().map(|value| format!("{{ value = {value}, weight = 0.0 }}")).collect::<Vec<_>>().join(", ");
                let wire = format!("kind = \"choice\"\nvalues = [{values}]\n");
                prop_assert!(toml::from_str::<Distribution<f32>>(&wire).is_err(), "wire={wire}");
            }
            InvalidF32Distribution::NonFiniteChoice(value) => {
                let choices = vec![WeightedValue::try_new(value, 1.0).unwrap()];
                let error = Distribution::<f32>::try_choice(choices).unwrap_err();
                assert!(matches!(error, InvalidDistributionError::NonFiniteValue { value: actual }
                    if actual.to_bits() == value.to_bits()), "value={value:?}");
                let wire = format!("kind = \"choice\"\nvalues = [{{ value = {}, weight = 1.0 }}]\n", toml_float(value));
                prop_assert!(toml::from_str::<Distribution<f32>>(&wire).is_err(), "wire={wire}");
            }
        }
    }

    #[test]
    fn invalid_asset_distributions_cannot_produce_a_validated_value(
        case in property_support::distributions::invalid_asset_distribution(),
    ) {
        match case {
            InvalidAssetDistribution::Uniform { minimum, maximum } => {
                let wire = serde_json::json!({"kind": "uniform", "min": minimum, "max": maximum});
                prop_assert!(serde_json::from_value::<Distribution<AssetId>>(wire.clone()).is_err(), "wire={wire}");
            }
            InvalidAssetDistribution::EmptyChoice => {
                prop_assert_eq!(Distribution::<AssetId>::try_choice(Vec::new()), Err(InvalidDistributionError::EmptyChoice));
                let wire = serde_json::json!({"kind": "choice", "values": []});
                prop_assert!(serde_json::from_value::<Distribution<AssetId>>(wire.clone()).is_err(), "wire={wire}");
            }
            InvalidAssetDistribution::AllZeroChoice(values) => {
                let entries = values.iter().map(|value| {
                    WeightedValue::try_new(AssetId::new(value.clone()).unwrap(), 0.0).unwrap()
                }).collect();
                prop_assert_eq!(Distribution::<AssetId>::try_choice(entries), Err(InvalidDistributionError::AllZeroChoice));
                let values = values.into_iter().map(|value| serde_json::json!({"value": value, "weight": 0.0})).collect::<Vec<_>>();
                let wire = serde_json::json!({"kind": "choice", "values": values});
                prop_assert!(serde_json::from_value::<Distribution<AssetId>>(wire.clone()).is_err(), "wire={wire}");
            }
            InvalidAssetDistribution::BlankFixed(value) => {
                prop_assert!(AssetId::new(value.clone()).is_none(), "value={:?}", value);
                let wire = serde_json::json!({"kind": "fixed", "value": value});
                prop_assert!(serde_json::from_value::<Distribution<AssetId>>(wire.clone()).is_err(), "wire={wire}");
            }
        }
    }
}

fn toml_float(value: f32) -> &'static str {
    if value.is_nan() {
        "nan"
    } else if value.is_sign_positive() {
        "inf"
    } else {
        "-inf"
    }
}

fn toml_weight(value: f64) -> &'static str {
    if value.is_nan() {
        "nan"
    } else if value.is_sign_positive() {
        "inf"
    } else {
        "-inf"
    }
}

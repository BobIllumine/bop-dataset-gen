//! Literal wire and validation contract tests for typed distributions.

use bop_core::{
    AssetId, Distribution, DistributionView, InvalidDistributionError, InvalidWeightedValueError,
    UniformBound, UniformSemantics, WeightedValue,
};

#[test]
fn fixed_uniform_and_choice_have_exact_json_and_toml_shapes() {
    let fixed = Distribution::<u32>::fixed(7);
    assert_eq!(
        serde_json::to_string(&fixed).unwrap(),
        r#"{"kind":"fixed","value":7}"#
    );
    assert_eq!(
        serde_json::from_str::<Distribution<u32>>(r#"{"kind":"fixed","value":7}"#).unwrap(),
        fixed
    );
    assert_eq!(
        toml::from_str::<Distribution<u32>>("kind = \"fixed\"\nvalue = 7\n").unwrap(),
        fixed
    );

    let uniform = Distribution::<u32>::try_uniform(2, 9).unwrap();
    assert_eq!(
        serde_json::to_string(&uniform).unwrap(),
        r#"{"kind":"uniform","min":2,"max":9}"#
    );
    assert_eq!(
        serde_json::from_str::<Distribution<u32>>(r#"{"kind":"uniform","min":2,"max":9}"#,)
            .unwrap(),
        uniform
    );
    assert_eq!(
        toml::from_str::<Distribution<u32>>("kind = \"uniform\"\nmin = 2\nmax = 9\n").unwrap(),
        uniform
    );

    let choice = Distribution::<u32>::try_choice(vec![
        WeightedValue::try_new(3, 0.25).unwrap(),
        WeightedValue::try_new(8, 0.75).unwrap(),
    ])
    .unwrap();
    assert_eq!(
        serde_json::to_string(&choice).unwrap(),
        r#"{"kind":"choice","values":[{"value":3,"weight":0.25},{"value":8,"weight":0.75}]}"#
    );
    assert_eq!(
        serde_json::from_str::<Distribution<u32>>(
            r#"{"kind":"choice","values":[{"value":3,"weight":0.25},{"value":8,"weight":0.75}]}"#,
        )
        .unwrap(),
        choice
    );
    assert_eq!(
        toml::from_str::<Distribution<u32>>(
            "kind = \"choice\"\nvalues = [{ value = 3, weight = 0.25 }, { value = 8, weight = 0.75 }]\n",
        )
        .unwrap(),
        choice
    );
}

#[test]
fn wire_shapes_reject_unknown_fields_at_distribution_and_weight_levels() {
    for json in [
        r#"{"kind":"fixed","value":7,"extra":true}"#,
        r#"{"kind":"uniform","min":2,"max":9,"extra":true}"#,
        r#"{"kind":"choice","values":[{"value":3,"weight":1.0}],"extra":true}"#,
        r#"{"kind":"choice","values":[{"value":3,"weight":1.0,"extra":true}]}"#,
    ] {
        assert!(serde_json::from_str::<Distribution<u32>>(json).is_err());
    }

    for toml in [
        "kind = \"fixed\"\nvalue = 7\nextra = true\n",
        "kind = \"uniform\"\nmin = 2\nmax = 9\nextra = true\n",
        "kind = \"choice\"\nvalues = [{ value = 3, weight = 1.0 }]\nextra = true\n",
        "kind = \"choice\"\nvalues = [{ value = 3, weight = 1.0, extra = true }]\n",
    ] {
        assert!(toml::from_str::<Distribution<u32>>(toml).is_err());
    }
}

#[test]
fn weighted_values_validate_weights_and_expose_read_only_fields() {
    let weighted = WeightedValue::try_new(String::from("part-a"), -0.0).unwrap();
    assert_eq!(weighted.value(), "part-a");
    assert_eq!(weighted.weight().to_bits(), 0.0_f64.to_bits());
    assert_eq!(
        serde_json::to_string(&weighted).unwrap(),
        r#"{"value":"part-a","weight":0.0}"#
    );
    assert_eq!(
        serde_json::from_str::<WeightedValue<String>>(r#"{"value":"part-a","weight":0.0}"#,)
            .unwrap(),
        weighted
    );
    assert_eq!(
        toml::from_str::<WeightedValue<String>>("value = \"part-a\"\nweight = 0.0\n").unwrap(),
        weighted
    );

    assert!(matches!(
        WeightedValue::try_new(1, -0.5),
        Err(InvalidWeightedValueError::NegativeWeight { weight })
            if weight.to_bits() == (-0.5_f64).to_bits()
    ));
    for weight in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            WeightedValue::try_new(1, weight),
            Err(InvalidWeightedValueError::NonFiniteWeight { weight: actual })
                if actual.to_bits() == weight.to_bits()
        ));
    }

    assert!(serde_json::from_str::<WeightedValue<u32>>(r#"{"value":1,"weight":-0.5}"#).is_err());
    assert!(serde_json::from_str::<WeightedValue<u32>>(r#"{"value":1,"weight":1e400}"#).is_err());
    assert!(toml::from_str::<WeightedValue<u32>>("value = 1\nweight = nan\n").is_err());
    assert!(toml::from_str::<WeightedValue<u32>>("value = 1\nweight = -0.5\n").is_err());
    assert!(
        serde_json::from_str::<WeightedValue<u32>>(r#"{"value":1,"weight":1.0,"extra":true}"#,)
            .is_err()
    );
}

#[test]
fn integer_uniform_bounds_are_inclusive_and_allow_equality() {
    let ranged = Distribution::<u32>::try_uniform(0, u32::MAX).unwrap();
    assert!(matches!(
        ranged.view(),
        DistributionView::Uniform {
            min: &0,
            max: &u32::MAX,
            semantics: UniformSemantics::Inclusive,
        }
    ));

    let equal = Distribution::<u32>::try_uniform(11, 11).unwrap();
    assert!(matches!(
        equal.view(),
        DistributionView::Uniform {
            min: &11,
            max: &11,
            semantics: UniformSemantics::Inclusive,
        }
    ));
    assert_eq!(
        serde_json::from_str::<Distribution<u32>>(r#"{"kind":"uniform","min":11,"max":11}"#,)
            .unwrap(),
        equal
    );
    assert_eq!(
        toml::from_str::<Distribution<u32>>("kind = \"uniform\"\nmin = 11\nmax = 11\n").unwrap(),
        equal
    );

    assert_eq!(
        Distribution::<u32>::try_uniform(12, 11),
        Err(InvalidDistributionError::InvertedUniformBounds)
    );
    assert!(
        serde_json::from_str::<Distribution<u32>>(r#"{"kind":"uniform","min":12,"max":11}"#,)
            .is_err()
    );
    assert!(
        toml::from_str::<Distribution<u32>>("kind = \"uniform\"\nmin = 12\nmax = 11\n").is_err()
    );
}

#[test]
fn float_distributions_validate_values_and_report_interval_semantics() {
    let fixed = Distribution::<f32>::try_fixed(1.25).unwrap();
    assert!(matches!(
        fixed.view(),
        DistributionView::Fixed { value } if value.to_bits() == 1.25_f32.to_bits()
    ));
    assert_eq!(
        serde_json::to_string(&fixed).unwrap(),
        r#"{"kind":"fixed","value":1.25}"#
    );
    assert_eq!(
        toml::from_str::<Distribution<f32>>("kind = \"fixed\"\nvalue = 1.25\n").unwrap(),
        fixed
    );

    let ranged = Distribution::<f32>::try_uniform(-1.5, 2.5).unwrap();
    assert!(matches!(
        ranged.view(),
        DistributionView::Uniform {
            min,
            max,
            semantics: UniformSemantics::ClosedOpen,
        } if min.to_bits() == (-1.5_f32).to_bits() && max.to_bits() == 2.5_f32.to_bits()
    ));
    assert_eq!(
        serde_json::from_str::<Distribution<f32>>(r#"{"kind":"uniform","min":-1.5,"max":2.5}"#,)
            .unwrap(),
        ranged
    );
    assert_eq!(
        toml::from_str::<Distribution<f32>>("kind = \"uniform\"\nmin = -1.5\nmax = 2.5\n",)
            .unwrap(),
        ranged
    );

    let equal = Distribution::<f32>::try_uniform(3.0, 3.0).unwrap();
    assert!(matches!(
        equal.view(),
        DistributionView::Uniform {
            min,
            max,
            semantics: UniformSemantics::Fixed,
        } if min.to_bits() == 3.0_f32.to_bits() && max.to_bits() == 3.0_f32.to_bits()
    ));
    assert_eq!(
        serde_json::from_str::<Distribution<f32>>(r#"{"kind":"uniform","min":3.0,"max":3.0}"#,)
            .unwrap(),
        equal
    );
    assert_eq!(
        toml::from_str::<Distribution<f32>>("kind = \"uniform\"\nmin = 3.0\nmax = 3.0\n").unwrap(),
        equal
    );

    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(matches!(
            Distribution::<f32>::try_fixed(value),
            Err(InvalidDistributionError::NonFiniteValue { value: actual })
                if actual.to_bits() == value.to_bits()
        ));
    }
    for (min, max, bound) in [
        (f32::NAN, 1.0, UniformBound::Minimum),
        (f32::NEG_INFINITY, 1.0, UniformBound::Minimum),
        (0.0, f32::INFINITY, UniformBound::Maximum),
    ] {
        assert!(matches!(
            Distribution::<f32>::try_uniform(min, max),
            Err(InvalidDistributionError::NonFiniteUniformBound {
                bound: actual_bound,
                value,
            }) if actual_bound == bound
                && value.to_bits() == if bound == UniformBound::Minimum {
                    min.to_bits()
                } else {
                    max.to_bits()
                }
        ));
    }
    assert_eq!(
        Distribution::<f32>::try_uniform(2.0, 1.0),
        Err(InvalidDistributionError::InvertedUniformBounds)
    );

    assert!(
        serde_json::from_str::<Distribution<f32>>(r#"{"kind":"uniform","min":2.0,"max":1.0}"#,)
            .is_err()
    );
    assert!(
        serde_json::from_str::<Distribution<f32>>(r#"{"kind":"uniform","min":1e400,"max":2.0}"#,)
            .is_err()
    );
    assert!(
        toml::from_str::<Distribution<f32>>("kind = \"uniform\"\nmin = nan\nmax = 2.0\n").is_err()
    );
    assert!(
        toml::from_str::<Distribution<f32>>("kind = \"uniform\"\nmin = 2.0\nmax = 1.0\n").is_err()
    );
}

#[test]
fn choices_reject_empty_all_zero_and_nonfinite_values_but_allow_mixed_zero_weight() {
    assert_eq!(
        Distribution::<u32>::try_choice(Vec::new()),
        Err(InvalidDistributionError::EmptyChoice)
    );
    assert_eq!(
        Distribution::<u32>::try_choice(vec![
            WeightedValue::try_new(1, 0.0).unwrap(),
            WeightedValue::try_new(2, -0.0).unwrap(),
        ]),
        Err(InvalidDistributionError::AllZeroChoice)
    );
    assert!(serde_json::from_str::<Distribution<u32>>(r#"{"kind":"choice","values":[]}"#).is_err());
    assert!(toml::from_str::<Distribution<u32>>("kind = \"choice\"\nvalues = []\n").is_err());
    assert!(
        serde_json::from_str::<Distribution<u32>>(
            r#"{"kind":"choice","values":[{"value":1,"weight":0.0}]}"#,
        )
        .is_err()
    );
    assert!(
        toml::from_str::<Distribution<u32>>(
            "kind = \"choice\"\nvalues = [{ value = 1, weight = 0.0 }]\n",
        )
        .is_err()
    );

    let choice = Distribution::<f32>::try_choice(vec![
        WeightedValue::try_new(1.0, 0.0).unwrap(),
        WeightedValue::try_new(2.0, 1.0).unwrap(),
    ])
    .unwrap();
    assert!(matches!(
        choice.view(),
        DistributionView::Choice { values }
            if values.len() == 2
                && values[0].weight().to_bits() == 0.0_f64.to_bits()
                && values[1].weight().to_bits() == 1.0_f64.to_bits()
    ));
    assert_eq!(
        serde_json::from_str::<Distribution<f32>>(
            r#"{"kind":"choice","values":[{"value":1.0,"weight":0.0},{"value":2.0,"weight":1.0}]}"#,
        )
        .unwrap(),
        choice
    );
    assert_eq!(
        toml::from_str::<Distribution<f32>>(
            "kind = \"choice\"\nvalues = [{ value = 1.0, weight = 0.0 }, { value = 2.0, weight = 1.0 }]\n",
        )
        .unwrap(),
        choice
    );

    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(matches!(
            Distribution::<f32>::try_choice(vec![
                WeightedValue::try_new(value, 1.0).unwrap(),
            ]),
            Err(InvalidDistributionError::NonFiniteValue { value: actual })
                if actual.to_bits() == value.to_bits()
        ));
    }
    assert!(
        toml::from_str::<Distribution<f32>>(
            "kind = \"choice\"\nvalues = [{ value = inf, weight = 1.0 }]\n",
        )
        .is_err()
    );
}

#[test]
fn asset_distributions_support_fixed_and_choice_but_reject_uniform_wire_input() {
    let fixed = Distribution::<AssetId>::fixed(AssetId::new("part-a").unwrap());
    assert_eq!(
        serde_json::to_string(&fixed).unwrap(),
        r#"{"kind":"fixed","value":"part-a"}"#
    );
    assert_eq!(
        serde_json::from_str::<Distribution<AssetId>>(r#"{"kind":"fixed","value":"part-a"}"#,)
            .unwrap(),
        fixed
    );
    assert_eq!(
        toml::from_str::<Distribution<AssetId>>("kind = \"fixed\"\nvalue = \"part-a\"\n").unwrap(),
        fixed
    );

    let choice = Distribution::<AssetId>::try_choice(vec![
        WeightedValue::try_new(AssetId::new("part-a").unwrap(), 0.0).unwrap(),
        WeightedValue::try_new(AssetId::new("part-b").unwrap(), 2.0).unwrap(),
    ])
    .unwrap();
    assert_eq!(
        serde_json::to_string(&choice).unwrap(),
        r#"{"kind":"choice","values":[{"value":"part-a","weight":0.0},{"value":"part-b","weight":2.0}]}"#
    );
    assert_eq!(
        serde_json::from_str::<Distribution<AssetId>>(
            r#"{"kind":"choice","values":[{"value":"part-a","weight":0.0},{"value":"part-b","weight":2.0}]}"#,
        )
        .unwrap(),
        choice
    );
    assert_eq!(
        toml::from_str::<Distribution<AssetId>>(
            "kind = \"choice\"\nvalues = [{ value = \"part-a\", weight = 0.0 }, { value = \"part-b\", weight = 2.0 }]\n",
        )
        .unwrap(),
        choice
    );

    assert!(
        serde_json::from_str::<Distribution<AssetId>>(
            r#"{"kind":"uniform","min":"part-a","max":"part-b"}"#,
        )
        .is_err()
    );
    assert!(
        toml::from_str::<Distribution<AssetId>>(
            "kind = \"uniform\"\nmin = \"part-a\"\nmax = \"part-b\"\n",
        )
        .is_err()
    );
}

#[test]
fn distribution_errors_are_standard_errors() {
    fn assert_error<T: std::error::Error>() {}

    assert_error::<InvalidDistributionError>();
    assert_error::<InvalidWeightedValueError>();
}

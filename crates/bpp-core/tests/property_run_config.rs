//! Bounded generative tests for strict v1 run-configuration contracts.

mod property_support;

use bpp_core::{
    AssetId, Axis, CameraId, CameraPose, CameraProjection, ConfigCountField, ConfigPathField,
    ConfigTextField, DistributionView, RunConfig, RunConfigError, RunConfigValidationError,
};
use property_support::run_config::{InvalidConfigCase, ObjectCountCase, ValidConfigCase};
use proptest::prelude::*;

fn distribution_maximum(distribution: &bpp_core::Distribution<u32>) -> u32 {
    match distribution.view() {
        DistributionView::Fixed { value } => *value,
        DistributionView::Uniform { max, .. } => *max,
        DistributionView::Choice { values } => values
            .iter()
            .filter(|value| value.weight() > 0.0)
            .map(|value| *value.value())
            .max()
            .expect("validated choice has a positive weight"),
    }
}

#[allow(clippy::too_many_lines)]
fn assert_resolved_plan(case: &ValidConfigCase, plan: &bpp_core::RunPlan) {
    let asset_ids = plan
        .assets()
        .iter()
        .map(|asset| asset.id().as_str())
        .collect::<Vec<_>>();
    let expected_asset_ids = case.asset_ids();
    assert_eq!(
        asset_ids,
        expected_asset_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    assert!(asset_ids.windows(2).all(|ids| ids[0] < ids[1]));
    for asset in plan.assets() {
        let expected = if asset.id().as_str() == "asset-00" {
            case.positive_asset_weight
        } else {
            case.other_asset_weight
        };
        assert_eq!(asset.mixture_weight().to_bits(), expected.to_bits());
    }

    let camera_ids = plan
        .cameras()
        .iter()
        .map(|camera| camera.id().get())
        .collect::<Vec<_>>();
    assert_eq!(camera_ids, case.camera_ids());
    assert!(camera_ids.windows(2).all(|ids| ids[0] < ids[1]));

    assert_eq!(plan.run().master_seed(), case.master_seed);
    assert_eq!(
        plan.run().scene_attempt_limit(),
        if case.run_overrides { 7 } else { 20 }
    );
    assert_eq!(
        plan.run().placement_attempt_limit_per_object(),
        if case.run_overrides { 11 } else { 100 }
    );

    if let Some(physics) = case.physics_overrides {
        assert_eq!(
            plan.physics().fixed_timestep_s().to_bits(),
            physics.fixed_timestep_s.to_bits()
        );
        assert_eq!(plan.physics().min_steps(), physics.min_steps);
        assert_eq!(plan.physics().max_steps(), physics.max_steps);
        assert_eq!(
            plan.physics().stability_check_interval(),
            physics.stability_check_interval
        );
        assert_eq!(
            plan.physics().required_stable_checks(),
            physics.required_stable_checks
        );
        assert_eq!(
            plan.physics().linear_sleep_threshold_mps().to_bits(),
            physics.linear_sleep_threshold_mps.to_bits()
        );
        assert_eq!(
            plan.physics().angular_sleep_threshold_rad_s().to_bits(),
            physics.angular_sleep_threshold_rad_s.to_bits()
        );
    } else {
        assert_eq!(
            plan.physics().fixed_timestep_s().to_bits(),
            (1.0_f32 / 240.0).to_bits()
        );
        assert_eq!(plan.physics().min_steps(), 120);
        assert_eq!(plan.physics().max_steps(), 4_800);
        assert_eq!(plan.physics().stability_check_interval(), 24);
        assert_eq!(plan.physics().required_stable_checks(), 10);
        assert_eq!(
            plan.physics().linear_sleep_threshold_mps().to_bits(),
            0.01_f32.to_bits()
        );
        assert_eq!(
            plan.physics().angular_sleep_threshold_rad_s().to_bits(),
            0.05_f32.to_bits()
        );
    }

    let [bin_x, bin_y, bin_z] = plan
        .bin()
        .inner_dimensions_m()
        .map(bpp_core::PositiveMeters::get);
    assert_eq!(
        [bin_x, bin_y, bin_z].map(f32::to_bits),
        case.bin_dimensions_m.map(f32::to_bits)
    );
    let spawn = plan.population().spawn_volume();
    let [min_x, min_y, min_z] = spawn.min_m();
    let [max_x, max_y, max_z] = spawn.max_m();
    assert!(min_x >= -bin_x / 2.0 && max_x <= bin_x / 2.0 && min_x < max_x);
    assert!(min_y >= -bin_y / 2.0 && max_y <= bin_y / 2.0 && min_y < max_y);
    assert!(min_z >= 0.0 && max_z <= bin_z && min_z < max_z);
    assert_eq!((spawn.min_m(), spawn.max_m()), case.spawn_bounds());

    assert_eq!(
        distribution_maximum(plan.population().object_count()),
        case.object_count.maximum()
    );
    match (case.object_count, plan.population().object_count().view()) {
        (ObjectCountCase::Fixed(_), DistributionView::Fixed { .. })
        | (ObjectCountCase::Uniform { .. }, DistributionView::Uniform { .. })
        | (ObjectCountCase::Choice { .. }, DistributionView::Choice { .. }) => {}
        _ => panic!("object-count form changed for {case:?}"),
    }
    assert_eq!(
        matches!(
            plan.population().asset_mixture().view(),
            DistributionView::Choice { .. }
        ),
        case.asset_mixture_is_choice
    );

    let channels = plan.output_channels();
    assert_eq!(channels.rgb(), case.output_rgb);
    assert_eq!(channels.depth(), case.output_depth);
    assert_eq!(channels.instance(), case.output_instances);
    if channels.instance() {
        assert!(u16::try_from(distribution_maximum(plan.population().object_count())).is_ok());
    }

    for camera in plan.cameras() {
        let id = camera.id().get();
        assert_eq!(
            matches!(camera.projection(), CameraProjection::VerticalFov(_)),
            case.camera_projection_is_fov(id)
        );
        assert_eq!(
            matches!(camera.pose(), CameraPose::FixedWorldFromCamera(_)),
            case.camera_pose_is_fixed(id)
        );
    }
}

fn expected_invalid(case: InvalidConfigCase) -> Option<RunConfigValidationError> {
    match case {
        InvalidConfigCase::ZeroValidSceneCount => Some(RunConfigValidationError::ZeroCount {
            field: ConfigCountField::ValidSceneCount,
        }),
        InvalidConfigCase::ZeroSimulationWorkers => Some(RunConfigValidationError::ZeroCount {
            field: ConfigCountField::SimulationWorkers,
        }),
        InvalidConfigCase::DuplicateAssetId => Some(RunConfigValidationError::DuplicateAssetId {
            asset_id: AssetId::new("asset-00").unwrap(),
        }),
        InvalidConfigCase::DuplicateCameraId => Some(RunConfigValidationError::DuplicateCameraId {
            camera_id: CameraId::new(1),
        }),
        InvalidConfigCase::UnknownAssetReference => {
            Some(RunConfigValidationError::UnknownAssetReference {
                asset_id: AssetId::new("missing").unwrap(),
            })
        }
        InvalidConfigCase::AllZeroAssetWeights => {
            Some(RunConfigValidationError::AllZeroAssetWeights)
        }
        InvalidConfigCase::ZeroObjectCount => {
            Some(RunConfigValidationError::InvalidPopulationCount {
                minimum: 0,
                maximum: 0,
            })
        }
        InvalidConfigCase::ImpossibleObjectCount => {
            Some(RunConfigValidationError::InvalidPopulationCount {
                minimum: 5,
                maximum: 2,
            })
        }
        InvalidConfigCase::SpawnInverted => {
            Some(RunConfigValidationError::InvalidSpawnAxis { axis: Axis::X })
        }
        InvalidConfigCase::SpawnOutsideBin => {
            Some(RunConfigValidationError::SpawnOutsideBin { axis: Axis::X })
        }
        InvalidConfigCase::PhysicsOrdering => {
            Some(RunConfigValidationError::InvalidPhysicsOrdering {
                min_steps: 100,
                max_steps: 10,
            })
        }
        InvalidConfigCase::StabilityWindow => {
            Some(RunConfigValidationError::StabilityWindowExceedsMaximum {
                interval: 8,
                required_checks: 3,
                max_steps: 20,
            })
        }
        InvalidConfigCase::ProjectionBoth => {
            Some(RunConfigValidationError::InvalidCameraProjectionSources {
                camera_id: CameraId::new(1),
                configured: 2,
            })
        }
        InvalidConfigCase::PoseMissing => {
            Some(RunConfigValidationError::InvalidCameraPoseSources {
                camera_id: CameraId::new(1),
                configured: 0,
            })
        }
        InvalidConfigCase::NoOutputChannels => Some(RunConfigValidationError::NoOutputChannels),
        InvalidConfigCase::VisibleInstanceOverflow => {
            Some(RunConfigValidationError::ExcessiveInstanceCount { maximum: 65_536 })
        }
        InvalidConfigCase::NestedUnknownField => None,
        InvalidConfigCase::WhitespaceOutputPath => Some(RunConfigValidationError::InvalidPath {
            field: ConfigPathField::Output,
            asset_id: None,
        }),
        InvalidConfigCase::WhitespaceAssetId => Some(RunConfigValidationError::InvalidAssetId {
            value: String::from("  "),
        }),
        InvalidConfigCase::WhitespaceMaterialLabel => Some(RunConfigValidationError::InvalidText {
            field: ConfigTextField::BinMaterialLabel,
        }),
        InvalidConfigCase::WhitespaceDeviceSelection => {
            Some(RunConfigValidationError::InvalidText {
                field: ConfigTextField::BlenderProcDeviceSelection,
            })
        }
    }
}

fn assert_invalid_case(case: InvalidConfigCase) {
    let toml = case.toml();
    match (RunConfig::from_toml(&toml), expected_invalid(case)) {
        (Err(RunConfigError::Validation(actual)), Some(expected)) => {
            assert_eq!(actual, expected, "case={case:?}\ntoml={toml}");
        }
        (Err(RunConfigError::Toml(_)), None) => {}
        (actual, expected) => {
            panic!("case={case:?}, expected={expected:?}, actual={actual:?}\ntoml={toml}")
        }
    }
}

proptest! {
    #![proptest_config(property_support::suite_config(32))]

    #[test]
    fn compact_valid_toml_resolves_all_generated_dimensions(
        case in property_support::run_config::valid_config(),
    ) {
        let toml = case.toml();
        let config = RunConfig::from_toml(&toml);
        prop_assert!(config.is_ok(), "case={:?}, error={:?}\ntoml={}", case, config.err(), toml);
        assert_resolved_plan(&case, config.unwrap().plan());
    }

    #[test]
    fn large_populations_remain_valid_without_instance_labels(
        case in property_support::run_config::valid_large_population_without_instances(),
    ) {
        let toml = case.toml();
        let config = RunConfig::from_toml(&toml);
        prop_assert!(config.is_ok(), "case={:?}, error={:?}\ntoml={}", case, config.err(), toml);
        let config = config.unwrap();
        prop_assert!(!config.plan().output_channels().instance(), "case={:?}", case);
        prop_assert!(distribution_maximum(config.plan().population().object_count()) > u32::from(u16::MAX),
            "case={:?}", case);
        assert_resolved_plan(&case, config.plan());
    }

    #[test]
    fn isolated_invalid_mutations_reach_the_intended_boundary(
        case in property_support::run_config::invalid_config(),
    ) {
        assert_invalid_case(case);
    }
}

#[test]
fn every_invalid_mutation_has_deterministic_boundary_coverage() {
    for case in InvalidConfigCase::ALL {
        assert_invalid_case(case);
    }
}

#[cfg(feature = "schema")]
proptest! {
    #![proptest_config(property_support::suite_config(24))]

    #[test]
    fn generated_valid_toml_is_accepted_by_the_generated_schema(
        case in property_support::run_config::valid_config(),
    ) {
        let toml = case.toml();
        RunConfig::from_toml(&toml).expect("same generated case passes runtime validation");
        let schema: serde_json::Value = serde_json::from_str(&bpp_core::run_config_json_schema())
            .expect("generated schema JSON");
        let validator = jsonschema::validator_for(&schema).expect("generated schema compiles");
        let value: toml::Value = toml::from_str(&toml).expect("generated TOML value");
        let instance = serde_json::to_value(value).expect("TOML converts to JSON");
        let errors = validator
            .iter_errors(&instance)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();

        prop_assert!(errors.is_empty(), "case={:?}, errors={:?}\ntoml={}", case, errors, toml);
    }
}

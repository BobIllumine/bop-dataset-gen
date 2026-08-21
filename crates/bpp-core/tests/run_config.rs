//! Public TOML run-configuration and cross-field validation contract tests.

use std::path::Path;

use bpp_core::{
    AssetId, Axis, CameraPose, CameraProjection, ConfigCountField, ConfigPathField,
    ConfigTextField, DistributionView, RunConfig, RunConfigError, RunConfigValidationError,
    WgpuAdapterPreference, WgpuDevicePreference,
};

const VALID_TOML: &str = r#"
[run]
master_seed = 123456789
valid_scene_count = 12
simulation_workers = 2
renderer_workers = 3
queue_capacity = 8
output_path = "output/dataset"
cache_path = "cache/assets"

[bin]
inner_dimensions_m = [1.0, 0.8, 0.5]
floor_thickness_m = 0.02
wall_thickness_m = 0.03
world_from_bin = { translation = [0.0, 0.0, 0.1], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
material_label = "galvanized_steel"
physical_properties = { friction = 0.55, restitution = 0.1 }

[[assets]]
id = "widget-z"
source_path = "assets/widget-z.glb"
source_unit_scale_m = 0.001
model_transform = { translation = [0.0, 0.0, 0.01], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
mixture_weight = 1.5
physical_properties = { mass_kg = { kind = "uniform", min = 0.1, max = 0.3 }, friction = { kind = "fixed", value = 0.5 }, restitution = { kind = "choice", values = [{ value = 0.1, weight = 1.0 }, { value = 0.2, weight = 2.0 }] } }
visual_override = { base_color_srgb = [0.8, 0.2, 0.1, 1.0], metallic = { kind = "fixed", value = 0.1 }, roughness = { kind = "uniform", min = 0.2, max = 0.7 } }
vhacd = { resolution = 100000, max_convex_hulls = 16, concavity = 0.0025, plane_downsampling = 4, convex_hull_downsampling = 4 }

[[assets]]
id = "part-a"
source_path = "assets/part-a.obj"
source_unit_scale_m = 1.0
model_transform = { translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
mixture_weight = 0.0
physical_properties = { mass_kg = { kind = "fixed", value = 0.2 }, friction = { kind = "uniform", min = 0.3, max = 0.8 }, restitution = { kind = "fixed", value = 0.05 } }
vhacd = { resolution = 50000, max_convex_hulls = 8, concavity = 0.005, plane_downsampling = 2, convex_hull_downsampling = 2 }

[population]
object_count = { kind = "choice", values = [{ value = 2, weight = 1.0 }, { value = 4, weight = 3.0 }] }
asset_mixture = { kind = "choice", values = [{ value = "widget-z", weight = 1.0 }, { value = "part-a", weight = 2.0 }] }
spawn_volume = { min_m = [-0.5, -0.4, 0.0], max_m = [0.5, 0.4, 0.5] }

[physics]
substeps = 2
solver_iterations = 8

[[cameras]]
id = 9
image_size = { width = 640, height = 480 }
near_m = 0.01
far_m = 20.0
projection = { vertical_fov_deg = 55.0 }
pose = { shell = { min_radius_m = 0.8, max_radius_m = 1.2, min_azimuth_deg = -180.0, max_azimuth_deg = 180.0, min_elevation_deg = 20.0, max_elevation_deg = 70.0, target_bin_m = [0.0, 0.0, 0.2] } }

[[cameras]]
id = 2
image_size = { width = 1280, height = 720 }
near_m = 0.02
far_m = 30.0
projection = { opencv_k = [900.0, 0.0, 640.0, 0.0, 900.0, 360.0, 0.0, 0.0, 1.0] }
pose = { fixed_world_from_camera = { translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] } }

[appearance]
background_srgb = [0.02, 0.03, 0.04, 1.0]
ambient = { color = [1.0, 1.0, 1.0], intensity = 0.15 }
directional_lights = [
  { direction_world = [1.0, 0.0, -2.0], color = [1.0, 0.9, 0.8], illuminance_lux = 1200.0 },
  { direction_world = [-1.0, 1.0, -1.0], color = [0.5, 0.6, 1.0], illuminance_lux = 400.0 },
]
point_lights = [
  { position_world_m = [0.2, -0.1, 1.3], color = [1.0, 1.0, 1.0], luminous_power_lm = 800.0 },
]
material_defaults = { base_color_srgb = [0.7, 0.7, 0.7, 1.0], metallic = { kind = "fixed", value = 0.0 }, roughness = { kind = "uniform", min = 0.35, max = 0.65 } }

[renderers.wgpu]
device_preference = "automatic"
adapter_preference = "high_performance"
shadow_map_size = 2048
rgb_fxaa = true

[renderers.blenderproc]
executable_path = "tools/blenderproc"
worker_path = "workers/render.py"
device_selection = "CUDA:0"
handshake_timeout_s = 30.0
request_timeout_s = 300.0
stderr_tail_capacity = 65536

[output]
rgb = true
depth = true
visible_instances = true
"#;

fn validation_error(toml: &str) -> RunConfigValidationError {
    match RunConfig::from_toml(toml) {
        Err(RunConfigError::Validation(error)) => error,
        other => panic!("expected semantic validation error, got {other:?}"),
    }
}

fn remove_between(input: &str, start: &str, end: &str, replacement: &str) -> String {
    let start_index = input.find(start).expect("fixture start marker");
    let end_index = input[start_index..]
        .find(end)
        .map(|offset| start_index + offset)
        .expect("fixture end marker");
    format!(
        "{}{}{}",
        &input[..start_index],
        replacement,
        &input[end_index..]
    )
}

fn assert_float(actual: f32, expected: f32) {
    assert_eq!(actual.to_bits(), expected.to_bits());
}

fn assert_float_array<const N: usize>(actual: [f32; N], expected: [f32; N]) {
    assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
}

fn assert_run_bin_and_assets(plan: &bpp_core::RunPlan) {
    assert_eq!(plan.run().master_seed(), 123_456_789);
    assert_eq!(plan.run().valid_scene_count(), 12);
    assert_eq!(plan.run().scene_attempt_limit(), 20);
    assert_eq!(plan.run().placement_attempt_limit_per_object(), 100);
    assert_eq!(plan.run().simulation_workers(), 2);
    assert_eq!(plan.run().renderer_workers(), 3);
    assert_eq!(plan.run().queue_capacity(), 8);
    assert_eq!(plan.run().output_path(), Path::new("output/dataset"));
    assert_eq!(plan.run().cache_path(), Path::new("cache/assets"));

    assert_float_array(
        plan.bin()
            .inner_dimensions_m()
            .map(bpp_core::PositiveMeters::get),
        [1.0, 0.8, 0.5],
    );
    assert_float(plan.bin().floor_thickness_m().get(), 0.02);
    assert_float(plan.bin().wall_thickness_m().get(), 0.03);
    assert_eq!(plan.bin().material_label(), "galvanized_steel");
    assert_float(plan.bin().physical_properties().friction(), 0.55);

    assert_eq!(
        plan.assets()
            .iter()
            .map(|asset| asset.id().as_str())
            .collect::<Vec<_>>(),
        ["part-a", "widget-z"]
    );
    assert_eq!(
        plan.assets()[0].source_path(),
        Path::new("assets/part-a.obj")
    );
    assert_float(plan.assets()[1].source_unit_scale_m().get(), 0.001);
    assert!(plan.assets()[0].visual_override().is_none());
    assert_float_array(
        plan.assets()[1]
            .visual_override()
            .expect("configured override")
            .base_color_srgb()
            .rgba(),
        [0.8, 0.2, 0.1, 1.0],
    );
    assert_eq!(plan.assets()[1].vhacd().resolution(), 100_000);
}

fn assert_population_physics_and_cameras(plan: &bpp_core::RunPlan) {
    assert!(matches!(
        plan.population().object_count().view(),
        DistributionView::Choice { values } if values.len() == 2
    ));
    assert_float_array(plan.population().spawn_volume().min_m(), [-0.5, -0.4, 0.0]);
    assert_float_array(plan.population().spawn_volume().max_m(), [0.5, 0.4, 0.5]);

    assert_float(plan.physics().fixed_timestep_s(), 1.0 / 240.0);
    assert_eq!(plan.physics().substeps(), 2);
    assert_eq!(plan.physics().solver_iterations(), 8);
    assert_eq!(plan.physics().min_steps(), 120);
    assert_eq!(plan.physics().max_steps(), 4_800);
    assert_eq!(plan.physics().stability_check_interval(), 24);
    assert_float(plan.physics().linear_sleep_threshold_mps(), 0.01);
    assert_float(plan.physics().angular_sleep_threshold_rad_s(), 0.05);
    assert_eq!(plan.physics().required_stable_checks(), 10);

    assert_eq!(
        plan.cameras()
            .iter()
            .map(|camera| camera.id().get())
            .collect::<Vec<_>>(),
        [2, 9]
    );
    assert!(matches!(
        plan.cameras()[0].projection(),
        CameraProjection::OpenCvK(_)
    ));
    assert!(matches!(
        plan.cameras()[0].pose(),
        CameraPose::FixedWorldFromCamera(_)
    ));
    assert!(matches!(
        plan.cameras()[1].projection(),
        CameraProjection::VerticalFov(_)
    ));
    assert!(matches!(plan.cameras()[1].pose(), CameraPose::Shell(_)));
}

fn assert_appearance_renderers_and_output(plan: &bpp_core::RunPlan) {
    assert_float_array(
        plan.appearance().snapshot().directional_lights()[0].direction_world(),
        [1.0 / 5.0_f32.sqrt(), 0.0, -2.0 / 5.0_f32.sqrt()],
    );
    assert_float_array(
        plan.appearance()
            .material_defaults()
            .base_color_srgb()
            .rgba(),
        [0.7, 0.7, 0.7, 1.0],
    );

    assert_eq!(
        plan.renderers().wgpu().device_preference(),
        WgpuDevicePreference::Automatic
    );
    assert_eq!(
        plan.renderers().wgpu().adapter_preference(),
        WgpuAdapterPreference::HighPerformance
    );
    assert_eq!(plan.renderers().wgpu().shadow_map_size(), 2048);
    assert!(plan.renderers().wgpu().rgb_fxaa());
    assert_eq!(
        plan.renderers().blenderproc().worker_path(),
        Path::new("workers/render.py")
    );
    assert_float(plan.renderers().blenderproc().request_timeout_s(), 300.0);

    assert!(plan.output_channels().rgb());
    assert!(plan.output_channels().depth());
    assert!(plan.output_channels().instance());
}

#[test]
fn literal_v1_toml_resolves_every_section_defaults_and_deterministic_orders() {
    let config = RunConfig::from_toml(VALID_TOML).expect("valid literal v1 config");
    let plan = config.plan();

    assert_run_bin_and_assets(plan);
    assert_population_physics_and_cameras(plan);
    assert_appearance_renderers_and_output(plan);

    let owned_plan = config.into_plan();
    assert_eq!(owned_plan.run().master_seed(), 123_456_789);
}

#[test]
fn explicit_run_and_physics_values_override_all_defaults() {
    let toml = VALID_TOML
        .replace(
            "valid_scene_count = 12",
            "valid_scene_count = 12\nscene_attempt_limit = 7\nplacement_attempt_limit_per_object = 11",
        )
        .replace(
            "[physics]\nsubsteps = 2\nsolver_iterations = 8",
            "[physics]\nfixed_timestep_s = 0.005\nsubsteps = 2\nsolver_iterations = 8\nmin_steps = 100\nmax_steps = 1000\nstability_check_interval = 20\nlinear_sleep_threshold_mps = 0.02\nangular_sleep_threshold_rad_s = 0.03\nrequired_stable_checks = 5",
        );
    let config = RunConfig::from_toml(&toml).unwrap();
    let plan = config.plan();

    assert_eq!(plan.run().scene_attempt_limit(), 7);
    assert_eq!(plan.run().placement_attempt_limit_per_object(), 11);
    assert_float(plan.physics().fixed_timestep_s(), 0.005);
    assert_eq!(plan.physics().min_steps(), 100);
    assert_eq!(plan.physics().max_steps(), 1_000);
    assert_eq!(plan.physics().stability_check_interval(), 20);
    assert_eq!(plan.physics().required_stable_checks(), 5);
}

#[test]
fn toml_shape_errors_are_distinct_from_semantic_validation_errors() {
    let syntax = RunConfig::from_toml("not valid = [").unwrap_err();
    assert!(matches!(syntax, RunConfigError::Toml(_)));

    let missing_required = VALID_TOML.replace("master_seed = 123456789\n", "");
    assert!(matches!(
        RunConfig::from_toml(&missing_required),
        Err(RunConfigError::Toml(_))
    ));

    for unknown in [
        VALID_TOML.replace(
            "shadow_map_size = 2048",
            "shadow_map_size = 2048\nunknown_wgpu_field = true",
        ),
        VALID_TOML.replace(
            "image_size = { width = 640, height = 480 }",
            "image_size = { width = 640, height = 480, bogus = 1 }",
        ),
        VALID_TOML.replace(
            "convex_hull_downsampling = 4 }",
            "convex_hull_downsampling = 4, bogus = 1 }",
        ),
    ] {
        assert!(matches!(
            RunConfig::from_toml(&unknown),
            Err(RunConfigError::Toml(_))
        ));
    }

    let semantic = VALID_TOML.replace("queue_capacity = 8", "queue_capacity = 0");
    assert_eq!(
        validation_error(&semantic),
        RunConfigValidationError::ZeroCount {
            field: ConfigCountField::QueueCapacity,
        }
    );
}

#[test]
fn assets_must_be_nonempty_unique_reachable_and_have_positive_total_weight() {
    let no_assets = format!(
        "assets = []\n{}",
        remove_between(VALID_TOML, "[[assets]]", "[population]", "")
    );
    assert_eq!(
        validation_error(&no_assets),
        RunConfigValidationError::EmptyAssets
    );

    let duplicate = VALID_TOML.replace("id = \"part-a\"", "id = \"widget-z\"");
    assert_eq!(
        validation_error(&duplicate),
        RunConfigValidationError::DuplicateAssetId {
            asset_id: AssetId::new("widget-z").unwrap(),
        }
    );

    let unknown = VALID_TOML.replace(
        "{ value = \"part-a\", weight = 2.0 }",
        "{ value = \"missing\", weight = 2.0 }",
    );
    assert_eq!(
        validation_error(&unknown),
        RunConfigValidationError::UnknownAssetReference {
            asset_id: AssetId::new("missing").unwrap(),
        }
    );

    let all_zero = VALID_TOML.replace("mixture_weight = 1.5", "mixture_weight = 0.0");
    assert_eq!(
        validation_error(&all_zero),
        RunConfigValidationError::AllZeroAssetWeights
    );
}

#[test]
fn population_counts_are_strictly_positive_for_every_distribution_variant() {
    for object_count in [
        "{ kind = \"fixed\", value = 0 }",
        "{ kind = \"uniform\", min = 0, max = 4 }",
        "{ kind = \"uniform\", min = 2, max = 0 }",
        "{ kind = \"choice\", values = [{ value = 0, weight = 1.0 }] }",
    ] {
        let toml = VALID_TOML.replace(
            "{ kind = \"choice\", values = [{ value = 2, weight = 1.0 }, { value = 4, weight = 3.0 }] }",
            object_count,
        );
        assert!(matches!(
            validation_error(&toml),
            RunConfigValidationError::InvalidPopulationCount { .. }
        ));
    }
}

#[test]
fn spawn_volume_is_strictly_nonempty_and_inside_centered_floor_origin_bin() {
    let inverted = VALID_TOML.replace(
        "min_m = [-0.5, -0.4, 0.0], max_m = [0.5, 0.4, 0.5]",
        "min_m = [0.2, -0.4, 0.0], max_m = [0.2, 0.4, 0.5]",
    );
    assert_eq!(
        validation_error(&inverted),
        RunConfigValidationError::InvalidSpawnAxis { axis: Axis::X }
    );

    for (bounds, axis) in [
        (
            "min_m = [-0.5001, -0.4, 0.0], max_m = [0.5, 0.4, 0.5]",
            Axis::X,
        ),
        (
            "min_m = [-0.5, -0.4, -0.001], max_m = [0.5, 0.4, 0.5]",
            Axis::Z,
        ),
        (
            "min_m = [-0.5, -0.4, 0.0], max_m = [0.5, 0.4001, 0.5]",
            Axis::Y,
        ),
    ] {
        let toml = VALID_TOML.replace("min_m = [-0.5, -0.4, 0.0], max_m = [0.5, 0.4, 0.5]", bounds);
        assert_eq!(
            validation_error(&toml),
            RunConfigValidationError::SpawnOutsideBin { axis }
        );
    }
}

#[test]
fn physics_ordering_and_stability_window_are_checked() {
    let ordering = VALID_TOML.replace(
        "solver_iterations = 8",
        "solver_iterations = 8\nmin_steps = 500\nmax_steps = 100",
    );
    assert_eq!(
        validation_error(&ordering),
        RunConfigValidationError::InvalidPhysicsOrdering {
            min_steps: 500,
            max_steps: 100,
        }
    );

    let impossible = VALID_TOML.replace(
        "solver_iterations = 8",
        "solver_iterations = 8\nmin_steps = 1\nmax_steps = 100\nstability_check_interval = 30\nrequired_stable_checks = 4",
    );
    assert_eq!(
        validation_error(&impossible),
        RunConfigValidationError::StabilityWindowExceedsMaximum {
            interval: 30,
            required_checks: 4,
            max_steps: 100,
        }
    );
}

#[test]
fn cameras_are_nonempty_unique_exclusive_and_sorted_only_by_camera_id() {
    let no_cameras = format!(
        "cameras = []\n{}",
        remove_between(VALID_TOML, "[[cameras]]", "[appearance]", "")
    );
    assert_eq!(
        validation_error(&no_cameras),
        RunConfigValidationError::EmptyCameras
    );

    let duplicate = VALID_TOML.replace("id = 2\nimage_size", "id = 9\nimage_size");
    assert_eq!(
        validation_error(&duplicate),
        RunConfigValidationError::DuplicateCameraId {
            camera_id: bpp_core::CameraId::new(9),
        }
    );

    let projection_both = VALID_TOML.replace(
        "projection = { vertical_fov_deg = 55.0 }",
        "projection = { vertical_fov_deg = 55.0, opencv_k = [900.0, 0.0, 320.0, 0.0, 900.0, 240.0, 0.0, 0.0, 1.0] }",
    );
    assert!(matches!(
        validation_error(&projection_both),
        RunConfigValidationError::InvalidCameraProjectionSources { configured: 2, .. }
    ));

    let projection_missing = VALID_TOML.replace(
        "projection = { vertical_fov_deg = 55.0 }",
        "projection = {}",
    );
    assert!(matches!(
        validation_error(&projection_missing),
        RunConfigValidationError::InvalidCameraProjectionSources { configured: 0, .. }
    ));

    let pose_both = VALID_TOML.replace(
        "pose = { fixed_world_from_camera = { translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] } }",
        "pose = { fixed_world_from_camera = { translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }, shell = { min_radius_m = 1.0, max_radius_m = 1.0, min_azimuth_deg = 0.0, max_azimuth_deg = 0.0, min_elevation_deg = 30.0, max_elevation_deg = 30.0, target_bin_m = [0.0, 0.0, 0.2] } }",
    );
    assert!(matches!(
        validation_error(&pose_both),
        RunConfigValidationError::InvalidCameraPoseSources { configured: 2, .. }
    ));

    let pose_missing = VALID_TOML.replace(
        "pose = { fixed_world_from_camera = { translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] } }",
        "pose = {}",
    );
    assert!(matches!(
        validation_error(&pose_missing),
        RunConfigValidationError::InvalidCameraPoseSources { configured: 0, .. }
    ));
}

#[test]
fn invalid_paths_text_and_work_counts_have_field_specific_errors() {
    for (needle, replacement, field) in [
        (
            "output_path = \"output/dataset\"",
            "output_path = \"   \"",
            ConfigPathField::Output,
        ),
        (
            "cache_path = \"cache/assets\"",
            "cache_path = \"\"",
            ConfigPathField::Cache,
        ),
        (
            "source_path = \"assets/widget-z.glb\"",
            "source_path = \" \"",
            ConfigPathField::AssetSource,
        ),
        (
            "worker_path = \"workers/render.py\"",
            "worker_path = \"\"",
            ConfigPathField::BlenderProcWorker,
        ),
    ] {
        let toml = VALID_TOML.replace(needle, replacement);
        assert!(matches!(
            validation_error(&toml),
            RunConfigValidationError::InvalidPath { field: actual, .. } if actual == field
        ));
    }

    for (needle, replacement, field) in [
        (
            "material_label = \"galvanized_steel\"",
            "material_label = \" \"",
            ConfigTextField::BinMaterialLabel,
        ),
        (
            "device_selection = \"CUDA:0\"",
            "device_selection = \"\"",
            ConfigTextField::BlenderProcDeviceSelection,
        ),
    ] {
        let toml = VALID_TOML.replace(needle, replacement);
        assert_eq!(
            validation_error(&toml),
            RunConfigValidationError::InvalidText { field }
        );
    }

    for (needle, replacement, field) in [
        (
            "simulation_workers = 2",
            "simulation_workers = 0",
            ConfigCountField::SimulationWorkers,
        ),
        (
            "renderer_workers = 3",
            "renderer_workers = 0",
            ConfigCountField::RendererWorkers,
        ),
        (
            "shadow_map_size = 2048",
            "shadow_map_size = 0",
            ConfigCountField::ShadowMapSize,
        ),
        (
            "stderr_tail_capacity = 65536",
            "stderr_tail_capacity = 0",
            ConfigCountField::StderrTailCapacity,
        ),
    ] {
        let toml = VALID_TOML.replace(needle, replacement);
        assert_eq!(
            validation_error(&toml),
            RunConfigValidationError::ZeroCount { field }
        );
    }
}

#[test]
fn output_requires_a_channel_and_u16_compatible_visible_instance_count() {
    let none = VALID_TOML
        .replace("rgb = true", "rgb = false")
        .replace("depth = true", "depth = false")
        .replace("visible_instances = true", "visible_instances = false");
    assert_eq!(
        validation_error(&none),
        RunConfigValidationError::NoOutputChannels
    );

    let excessive = VALID_TOML.replace(
        "{ kind = \"choice\", values = [{ value = 2, weight = 1.0 }, { value = 4, weight = 3.0 }] }",
        "{ kind = \"uniform\", min = 1, max = 65536 }",
    );
    assert_eq!(
        validation_error(&excessive),
        RunConfigValidationError::ExcessiveInstanceCount { maximum: 65_536 }
    );

    let excessive_without_instances =
        excessive.replace("visible_instances = true", "visible_instances = false");
    RunConfig::from_toml(&excessive_without_instances)
        .expect("large populations are allowed without visible-instance labels");

    let unreachable_large_choice = VALID_TOML.replace(
        "{ kind = \"choice\", values = [{ value = 2, weight = 1.0 }, { value = 4, weight = 3.0 }] }",
        "{ kind = \"choice\", values = [{ value = 4, weight = 1.0 }, { value = 65536, weight = 0.0 }] }",
    );
    RunConfig::from_toml(&unreachable_large_choice)
        .expect("a zero-weight count is valid but cannot exceed the possible label count");
}

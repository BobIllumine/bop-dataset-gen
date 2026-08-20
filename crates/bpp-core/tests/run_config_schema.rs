//! JSON Schema contract tests for the unchecked v1 run configuration input.

#![cfg(feature = "schema")]

use bpp_core::{RunConfig, run_config_json_schema};

const VALID_TOML: &str = r#"
[run]
master_seed = 1
valid_scene_count = 2
simulation_workers = 1
renderer_workers = 1
queue_capacity = 2
output_path = "output"
cache_path = "cache"

[bin]
inner_dimensions_m = [1.0, 0.8, 0.5]
floor_thickness_m = 0.02
wall_thickness_m = 0.03
world_from_bin = { translation = [0.0, 0.0, 0.1], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
material_label = "steel"
physical_properties = { friction = 0.5, restitution = 0.1 }

[[assets]]
id = "part"
source_path = "assets/part.glb"
source_unit_scale_m = 1.0
model_transform = { translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
mixture_weight = 1.0
physical_properties = { mass_kg = { kind = "uniform", min = 0.1, max = 0.3 }, friction = { kind = "fixed", value = 0.5 }, restitution = { kind = "choice", values = [{ value = 0.1, weight = 1.0 }] } }
visual_override = { base_color_srgb = [0.8, 0.2, 0.1, 1.0], metallic = { kind = "fixed", value = 0.1 }, roughness = { kind = "uniform", min = 0.2, max = 0.7 } }
vhacd = { resolution = 100000, max_convex_hulls = 16, concavity = 0.0025, plane_downsampling = 4, convex_hull_downsampling = 4 }

[population]
object_count = { kind = "fixed", value = 2 }
asset_mixture = { kind = "choice", values = [{ value = "part", weight = 1.0 }] }
spawn_volume = { min_m = [-0.5, -0.4, 0.0], max_m = [0.5, 0.4, 0.5] }

[physics]
substeps = 2
solver_iterations = 8

[[cameras]]
id = 1
image_size = { width = 640, height = 480 }
near_m = 0.01
far_m = 20.0
projection = { vertical_fov_deg = 55.0 }
pose = { shell = { min_radius_m = 0.8, max_radius_m = 1.2, min_azimuth_deg = -180.0, max_azimuth_deg = 180.0, min_elevation_deg = 20.0, max_elevation_deg = 70.0, target_bin_m = [0.0, 0.0, 0.2] } }

[appearance]
background_srgb = [0.02, 0.03, 0.04, 1.0]
ambient = { color = [1.0, 1.0, 1.0], intensity = 0.15 }
directional_lights = [{ direction_world = [1.0, 0.0, -2.0], color = [1.0, 0.9, 0.8], illuminance_lux = 1200.0 }]
point_lights = [{ position_world_m = [0.2, -0.1, 1.3], color = [1.0, 1.0, 1.0], luminous_power_lm = 800.0 }]
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

fn schema_and_instance() -> (serde_json::Value, serde_json::Value) {
    RunConfig::from_toml(VALID_TOML).expect("canonical TOML is accepted by RunConfig");
    let schema = serde_json::from_str(&run_config_json_schema()).expect("valid JSON Schema");
    let toml: toml::Value = toml::from_str(VALID_TOML).expect("valid canonical TOML");
    let instance = serde_json::to_value(toml).expect("TOML value converts to JSON");
    (schema, instance)
}

#[test]
fn generation_is_deterministic_parseable_and_strict_at_the_root() {
    let first = run_config_json_schema();
    let second = run_config_json_schema();

    assert_eq!(first, second);
    assert!(first.ends_with('\n'));
    assert!(!first.ends_with("\n\n"));

    let schema: serde_json::Value = serde_json::from_str(&first).expect("valid JSON Schema");
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["required"],
        serde_json::json!([
            "run",
            "bin",
            "assets",
            "population",
            "physics",
            "cameras",
            "appearance",
            "renderers",
            "output"
        ])
    );
}

#[test]
fn schema_marks_defaults_bounds_enums_and_distribution_tags() {
    let (schema, _) = schema_and_instance();
    let definitions = &schema["$defs"];
    let run = &definitions["RunWire"];

    assert_eq!(run["properties"]["scene_attempt_limit"]["default"], 20);
    assert_eq!(run["properties"]["scene_attempt_limit"]["minimum"], 1);
    assert_eq!(run["properties"]["scene_attempt_limit"]["type"], "integer");
    assert!(
        !run["required"]
            .as_array()
            .expect("required array")
            .iter()
            .any(|field| field == "scene_attempt_limit")
    );
    assert_eq!(run["properties"]["valid_scene_count"]["minimum"], 1);

    assert_eq!(
        definitions["WgpuDevicePreferenceWire"]["enum"],
        serde_json::json!(["automatic", "cpu", "gpu"])
    );
    assert_eq!(
        definitions["WgpuAdapterPreferenceWire"]["enum"],
        serde_json::json!(["automatic", "low_power", "high_performance"])
    );

    let tags = definitions["DistributionWire"]["oneOf"]
        .as_array()
        .expect("distribution variants")
        .iter()
        .map(|variant| variant["properties"]["kind"]["const"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(tags, [Some("fixed"), Some("uniform"), Some("choice")]);
    assert_eq!(
        definitions["WeightedValueWire"]["additionalProperties"],
        false
    );
}

#[test]
fn schema_marks_nonempty_collections_and_nonnegative_choice_weights() {
    let (schema, _) = schema_and_instance();
    let definitions = &schema["$defs"];

    assert_eq!(schema["properties"]["assets"]["minItems"], 1);
    assert_eq!(schema["properties"]["cameras"]["minItems"], 1);

    let mass_reference = definitions["PhysicalPropertyDistributionsWire"]["properties"]["mass_kg"]
        ["$ref"]
        .as_str()
        .expect("mass distribution reference");
    let mass_definition = mass_reference
        .rsplit('/')
        .next()
        .expect("definition name in reference");
    let choice = definitions[mass_definition]["oneOf"]
        .as_array()
        .expect("distribution variants")
        .iter()
        .find(|variant| variant["properties"]["kind"]["const"] == "choice")
        .expect("choice distribution");
    assert_eq!(choice["properties"]["values"]["minItems"], 1);

    let weighted_reference = choice["properties"]["values"]["items"]["$ref"]
        .as_str()
        .expect("weighted value reference");
    let weighted_definition = weighted_reference
        .rsplit('/')
        .next()
        .expect("weighted definition name in reference");
    let weight_reference = definitions[weighted_definition]["properties"]["weight"]["$ref"]
        .as_str()
        .expect("weight scalar reference");
    let weight_definition = weight_reference
        .rsplit('/')
        .next()
        .expect("weight definition name in reference");
    assert_eq!(definitions[weight_definition]["minimum"], 0.0);
}

#[test]
fn camera_projection_and_pose_forms_are_mutually_exclusive() {
    let (schema, mut instance) = schema_and_instance();
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");

    assert!(
        validator.is_valid(&instance),
        "canonical config errors: {:?}",
        validator.iter_errors(&instance).collect::<Vec<_>>()
    );

    instance["cameras"][0]["projection"]["opencv_k"] =
        serde_json::json!([500.0, 0.0, 320.0, 0.0, 500.0, 240.0, 0.0, 0.0, 1.0]);
    assert!(!validator.is_valid(&instance));

    instance["cameras"][0]["projection"] = serde_json::json!({"vertical_fov_deg": 55.0});
    instance["cameras"][0]["pose"] = serde_json::json!({});
    assert!(!validator.is_valid(&instance));
}

#[test]
fn schema_rejects_nested_unknowns_null_options_and_malformed_distributions() {
    let (schema, instance) = schema_and_instance();
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");

    let mut unknown = instance.clone();
    unknown["assets"][0]["vhacd"]["bogus"] = serde_json::json!(true);
    assert!(!validator.is_valid(&unknown));

    let mut null_default = instance.clone();
    null_default["run"]["scene_attempt_limit"] = serde_json::Value::Null;
    assert!(!validator.is_valid(&null_default));

    let mut malformed_distribution = instance;
    malformed_distribution["assets"][0]["physical_properties"]["mass_kg"] =
        serde_json::json!({"kind": "uniform", "value": 1.0});
    assert!(!validator.is_valid(&malformed_distribution));
}

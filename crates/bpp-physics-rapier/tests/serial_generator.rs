//! Public serial scene-generation integration contracts.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use bpp_assets::ConvexPart;
use bpp_core::PhysicsRejectionReason;
use bpp_core::{AssetId, RunConfig, SceneId};
use bpp_physics_rapier::{PreparedPhysicsAsset, SceneGenerationError, SerialPhysicsGenerator};

fn plan() -> bpp_core::RunPlan {
    parse_plan(&config_toml())
}

fn parse_plan(input: &str) -> bpp_core::RunPlan {
    RunConfig::from_toml(input)
        .expect("test configuration parses")
        .into_plan()
}

fn config_toml() -> String {
    r#"[run]
master_seed = 19
valid_scene_count = 1
scene_attempt_limit = 3
placement_attempt_limit_per_object = 8
simulation_workers = 1
renderer_workers = 1
queue_capacity = 1
output_path = "output"
cache_path = "cache"

[bin]
inner_dimensions_m = [1.0, 0.8, 0.5]
floor_thickness_m = 0.02
wall_thickness_m = 0.03
world_from_bin = { translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
material_label = "steel"
physical_properties = { friction = 0.5, restitution = 0.1 }

[[assets]]
id = "cube"
source_path = "cube.glb"
source_unit_scale_m = 1.0
model_transform = { translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
mixture_weight = 1.0
physical_properties = { mass_kg = { kind = "fixed", value = 0.5 }, friction = { kind = "fixed", value = 0.5 }, restitution = { kind = "fixed", value = 0.1 } }
vhacd = { resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }

[population]
object_count = { kind = "fixed", value = 1 }
asset_mixture = { kind = "fixed", value = "cube" }
spawn_volume = { min_m = [-0.2, -0.2, 0.15], max_m = [0.2, 0.2, 0.3] }

[physics]
fixed_timestep_s = 0.004166667
substeps = 2
solver_iterations = 8
min_steps = 16
max_steps = 1000
stability_check_interval = 4
linear_sleep_threshold_mps = 0.05
angular_sleep_threshold_rad_s = 0.05
required_stable_checks = 3
escape_margin_m = 0.002
maximum_penetration_m = 0.01

[[cameras]]
id = 1
image_size = { width = 16, height = 16 }
near_m = 0.01
far_m = 10.0
projection = { vertical_fov_deg = 55.0 }
pose = { fixed_world_from_camera = { translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] } }

[appearance]
background_srgb = [0.0, 0.0, 0.0, 1.0]
ambient = { color = [1.0, 1.0, 1.0], intensity = 0.0 }
directional_lights = []
point_lights = []
material_defaults = { base_color_srgb = [0.5, 0.5, 0.5, 1.0], metallic = { kind = "fixed", value = 0.0 }, roughness = { kind = "fixed", value = 0.5 } }

[renderers.wgpu]
device_preference = "automatic"
adapter_preference = "high_performance"
shadow_map_size = 16
rgb_fxaa = false

[renderers.blenderproc]
executable_path = "blenderproc"
worker_path = "worker.py"
device_selection = "CPU"
handshake_timeout_s = 1.0
request_timeout_s = 1.0
stderr_tail_capacity = 64

[output]
rgb = true
depth = false
visible_instances = false
"#
    .to_owned()
}

fn cube() -> PreparedPhysicsAsset {
    let part = ConvexPart::try_new(
        vec![
            [-0.05, -0.05, -0.05],
            [0.05, -0.05, -0.05],
            [0.05, 0.05, -0.05],
            [-0.05, 0.05, -0.05],
            [-0.05, -0.05, 0.05],
            [0.05, -0.05, 0.05],
            [0.05, 0.05, 0.05],
            [-0.05, 0.05, 0.05],
        ],
        vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ],
    )
    .expect("cube is checked");
    PreparedPhysicsAsset::try_new(&[part]).expect("cube prepares")
}

fn prepared_assets() -> BTreeMap<AssetId, Arc<PreparedPhysicsAsset>> {
    BTreeMap::from([(
        AssetId::new("cube").expect("valid asset ID"),
        Arc::new(cube()),
    )])
}

#[test]
fn generates_a_repeatable_valid_scene_snapshot_end_to_end() {
    let plan = plan();
    let generator =
        SerialPhysicsGenerator::try_new(&plan, prepared_assets()).expect("generator compiles");

    let first = generator
        .generate_scene(SceneId::new(7))
        .expect("scene settles and validates");
    let replay = generator
        .generate_scene(SceneId::new(7))
        .expect("replay settles and validates");

    assert_eq!(first, replay);
    assert_eq!(first.scene_id(), SceneId::new(7));
    assert_eq!(first.accepted_attempt().get(), 0);
    assert_eq!(first.instances().len(), 1);
    assert_eq!(first.instances()[0].instance_id().get(), 1);
    assert_eq!(first.physics().attempts(), 1);
    assert!(first.physics().steps() >= 16);
    assert_eq!(first.seed_fingerprint().len(), 64);
    assert!(
        first
            .seed_fingerprint()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
    assert_eq!(
        first.seed_fingerprint(),
        "c814f69765e8d46780cad9ec8587dd69ea713832bd060f7c2e819100e628d794"
    );
}

#[test]
fn reports_exact_spawn_saturation_counts_when_attempts_are_exhausted() {
    let input = config_toml()
        .replace(
            "object_count = { kind = \"fixed\", value = 1 }",
            "object_count = { kind = \"fixed\", value = 2 }",
        )
        .replace(
            "spawn_volume = { min_m = [-0.2, -0.2, 0.15], max_m = [0.2, 0.2, 0.3] }",
            "spawn_volume = { min_m = [-0.05, -0.05, 0.15], max_m = [0.05, 0.05, 0.25] }",
        );
    let plan = parse_plan(&input);
    let generator =
        SerialPhysicsGenerator::try_new(&plan, prepared_assets()).expect("generator compiles");

    let error = generator
        .generate_scene(SceneId::new(4))
        .expect_err("every placement attempt must saturate");

    assert!(matches!(
        error,
        SceneGenerationError::AttemptsExhausted {
            scene_id,
            attempts: 3,
            rejection_counts,
        } if scene_id == SceneId::new(4)
            && rejection_counts.len() == 1
            && rejection_counts[0].reason() == PhysicsRejectionReason::SpawnSaturation
            && rejection_counts[0].count() == 3
    ));
}

#[test]
fn reports_timeout_for_each_attempt_that_reaches_the_step_budget() {
    let input = config_toml()
        .replace("min_steps = 16", "min_steps = 1")
        .replace("max_steps = 1000", "max_steps = 1")
        .replace(
            "stability_check_interval = 4",
            "stability_check_interval = 1",
        )
        .replace("required_stable_checks = 3", "required_stable_checks = 1")
        .replace(
            "linear_sleep_threshold_mps = 0.05",
            "linear_sleep_threshold_mps = 0.0",
        )
        .replace(
            "angular_sleep_threshold_rad_s = 0.05",
            "angular_sleep_threshold_rad_s = 0.0",
        );
    let plan = parse_plan(&input);
    let generator =
        SerialPhysicsGenerator::try_new(&plan, prepared_assets()).expect("generator compiles");

    let error = generator
        .generate_scene(SceneId::new(5))
        .expect_err("a falling body cannot settle in one step");

    assert!(matches!(
        error,
        SceneGenerationError::AttemptsExhausted {
            attempts: 3,
            rejection_counts,
            ..
        } if rejection_counts.len() == 1
            && rejection_counts[0].reason() == PhysicsRejectionReason::Timeout
            && rejection_counts[0].count() == 3
    ));
}

#[test]
fn construction_rejects_a_missing_selectable_prepared_asset() {
    let plan = plan();

    assert!(matches!(
        SerialPhysicsGenerator::try_new(&plan, BTreeMap::new()),
        Err(SceneGenerationError::Placement(_))
    ));
}

fn corpus_plan() -> bpp_core::RunPlan {
    parse_plan(
        &config_toml()
            .replace("scene_attempt_limit = 3", "scene_attempt_limit = 20")
            .replace(
                "placement_attempt_limit_per_object = 8",
                "placement_attempt_limit_per_object = 500",
            )
            .replace(
                "object_count = { kind = \"fixed\", value = 1 }",
                "object_count = { kind = \"fixed\", value = 4 }",
            ),
    )
}

fn generate_corpus(generator: &SerialPhysicsGenerator) -> Vec<bpp_core::SceneSnapshot> {
    (0..8)
        .map(|scene| {
            generator
                .generate_scene(SceneId::new(scene))
                .expect("primitive corpus scene is accepted")
        })
        .collect()
}

#[test]
fn eight_scene_primitive_corpus_is_locked_serializable_and_schedule_independent() {
    let plan = corpus_plan();
    let generator = Arc::new(
        SerialPhysicsGenerator::try_new(&plan, prepared_assets()).expect("generator compiles"),
    );
    let serial = generate_corpus(&generator);

    let mut scheduled = (0..8)
        .rev()
        .map(|scene| {
            let generator = Arc::clone(&generator);
            std::thread::spawn(move || {
                generator
                    .generate_scene(SceneId::new(scene))
                    .expect("scheduled primitive scene is accepted")
            })
        })
        .map(|handle| handle.join().expect("scene worker does not panic"))
        .collect::<Vec<_>>();
    scheduled.sort_by_key(bpp_core::SceneSnapshot::scene_id);

    assert_eq!(scheduled, serial);
    assert!(serial.iter().all(|scene| scene.instances().len() == 4));
    for scene in &serial {
        let json = serde_json::to_vec(scene).expect("scene serializes");
        let decoded: bpp_core::SceneSnapshot =
            serde_json::from_slice(&json).expect("scene deserializes");
        assert_scene_round_trip(scene, &decoded);
    }
    let bytes = serde_json::to_vec(&serial).expect("corpus serializes canonically");
    let digest = blake3::hash(&bytes).to_hex().to_string();

    #[cfg(feature = "fast")]
    assert_eq!(
        digest,
        "d390de9fc435c2f446accdeb91b5e0d9d3b8c409bd3554ee8b36d977f24b1ecd"
    );
    #[cfg(feature = "reproducible")]
    assert_eq!(
        digest,
        "7d1e7defc0a3fb83f8aa6a6a0f34eae94065dd3d29264b4dc0bc315d4c189100"
    );
}

fn assert_scene_round_trip(expected: &bpp_core::SceneSnapshot, decoded: &bpp_core::SceneSnapshot) {
    assert_eq!(decoded.scene_id(), expected.scene_id());
    assert_eq!(decoded.accepted_attempt(), expected.accepted_attempt());
    assert_eq!(decoded.seed_fingerprint(), expected.seed_fingerprint());
    assert_eq!(decoded.bin(), expected.bin());
    assert_eq!(decoded.physics(), expected.physics());
    assert_eq!(decoded.instances().len(), expected.instances().len());
    for (decoded, expected) in decoded.instances().iter().zip(expected.instances()) {
        assert_eq!(decoded.instance_id(), expected.instance_id());
        assert_eq!(decoded.asset_id(), expected.asset_id());
        assert_eq!(
            decoded.physical_properties(),
            expected.physical_properties()
        );
        assert_eq!(
            decoded
                .world_from_model()
                .translation()
                .map(bpp_core::Meters::get)
                .map(f32::to_bits),
            expected
                .world_from_model()
                .translation()
                .map(bpp_core::Meters::get)
                .map(f32::to_bits)
        );
        let decoded_rotation = decoded.world_from_model().rotation_xyzw();
        let expected_rotation = expected.world_from_model().rotation_xyzw();
        assert!(
            decoded_rotation
                .into_iter()
                .zip(expected_rotation)
                .all(|(left, right)| (left - right).abs() <= 2.0 * f32::EPSILON)
        );
    }
}

#[test]
#[ignore = "manual non-gating release serial-generator timing"]
fn measure_eight_scene_four_object_primitive_corpus() {
    let plan = corpus_plan();
    let generator =
        SerialPhysicsGenerator::try_new(&plan, prepared_assets()).expect("generator compiles");
    let mut durations = Vec::with_capacity(5);
    for _ in 0..5 {
        let started = Instant::now();
        let scenes = generate_corpus(&generator);
        assert_eq!(scenes.len(), 8);
        durations.push(started.elapsed());
    }
    durations.sort_unstable();
    let median = durations[2];
    let scenes_per_second = 8.0 / median.as_secs_f64();
    eprintln!("serial primitive corpus median={median:?}, scenes/s={scenes_per_second:.2}");
    assert!(median < Duration::from_secs(30));
}

//! Public settling contracts for the serial Rapier physics world.

use bop_assets::ConvexPart;
use bop_core::{
    AssetId, InstanceId, ResolvedPhysicalProperties, RigidTransform, RunConfig, RunPlan,
};
use bop_physics_rapier::{DynamicBodySpec, PhysicsWorld, PreparedPhysicsAsset, SettlingOutcome};
use std::time::{Duration, Instant};

fn plan(physics: &str) -> RunPlan {
    RunConfig::from_toml(&format!(
        r#"[run]
master_seed = 1
valid_scene_count = 1
simulation_workers = 1
renderer_workers = 1
queue_capacity = 1
output_path = "output"
cache_path = "cache"

[bin]
inner_dimensions_m = [1.0, 0.8, 0.5]
floor_thickness_m = 0.02
wall_thickness_m = 0.03
world_from_bin = {{ translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
material_label = "steel"
physical_properties = {{ friction = 0.5, restitution = 0.1 }}

[[assets]]
id = "cube"
source_path = "cube.glb"
source_unit_scale_m = 1.0
model_transform = {{ translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
mixture_weight = 1.0
physical_properties = {{ mass_kg = {{ kind = "fixed", value = 0.5 }}, friction = {{ kind = "fixed", value = 0.5 }}, restitution = {{ kind = "fixed", value = 0.1 }} }}
vhacd = {{ resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }}

[population]
object_count = {{ kind = "fixed", value = 1 }}
asset_mixture = {{ kind = "fixed", value = "cube" }}
spawn_volume = {{ min_m = [-0.25, -0.2, 0.0], max_m = [0.25, 0.2, 0.25] }}

[physics]
{physics}

[[cameras]]
id = 1
image_size = {{ width = 16, height = 16 }}
near_m = 0.01
far_m = 10.0
projection = {{ vertical_fov_deg = 55.0 }}
pose = {{ fixed_world_from_camera = {{ translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }} }}

[appearance]
background_srgb = [0.0, 0.0, 0.0, 1.0]
ambient = {{ color = [1.0, 1.0, 1.0], intensity = 0.0 }}
directional_lights = []
point_lights = []
material_defaults = {{ base_color_srgb = [0.5, 0.5, 0.5, 1.0], metallic = {{ kind = "fixed", value = 0.0 }}, roughness = {{ kind = "fixed", value = 0.5 }} }}

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
    ))
    .expect("test configuration parses")
    .into_plan()
}

fn cube() -> ConvexPart {
    ConvexPart::try_new(
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
    .expect("cube is checked")
}

fn populated_world(plan: &RunPlan) -> PhysicsWorld {
    populated_world_with_count(plan, 1)
}

fn populated_world_with_count(plan: &RunPlan, count: u16) -> PhysicsWorld {
    let asset_id = AssetId::new("cube").expect("nonempty asset ID");
    let asset = PreparedPhysicsAsset::try_new(&[cube()]).expect("cube prepares once");
    let properties = ResolvedPhysicalProperties::try_new(0.5, 0.5, 0.1).expect("valid properties");
    let mut world = PhysicsWorld::try_new(plan.bin()).expect("finite bin constructs");
    let bodies = (1..=count)
        .map(|value| {
            let column = f32::from((value - 1) % 4);
            let row = f32::from((value - 1) / 4);
            DynamicBodySpec::new(
                InstanceId::new(value).expect("nonzero instance ID"),
                &asset_id,
                &asset,
                RigidTransform::try_new(
                    [-0.15 + 0.1 * column, -0.15 + 0.1 * row, 0.25 + 0.12 * row],
                    [0.0, 0.0, 0.0, 1.0],
                )
                .expect("finite pose"),
                properties,
            )
        })
        .collect::<Vec<_>>();
    world
        .insert_dynamic_bodies(&bodies)
        .expect("checked cube inserts");
    world
}

#[test]
fn settle_returns_an_outcome_with_a_logical_step_accessor() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 1\nsolver_iterations = 1\nmin_steps = 1\nmax_steps = 1\nstability_check_interval = 1\nlinear_sleep_threshold_mps = 100.0\nangular_sleep_threshold_rad_s = 100.0\nrequired_stable_checks = 1",
    );
    let mut world = populated_world(&plan);

    let outcome = world
        .settle(*plan.physics())
        .expect("checked settling configuration advances the world");

    assert_eq!(outcome.steps(), 1);
    assert!(matches!(outcome, SettlingOutcome::Settled { .. }));
}

#[test]
fn one_logical_step_conserves_its_timestep_across_four_substeps() {
    let plan = plan(
        "fixed_timestep_s = 0.1\nsubsteps = 4\nsolver_iterations = 1\nmin_steps = 1\nmax_steps = 1\nstability_check_interval = 1\nlinear_sleep_threshold_mps = 100.0\nangular_sleep_threshold_rad_s = 100.0\nrequired_stable_checks = 1",
    );
    let mut world = populated_world(&plan);

    let outcome = world
        .settle(*plan.physics())
        .expect("checked settling configuration advances the world");
    let pose = world
        .body_pose(InstanceId::new(1).expect("nonzero instance ID"))
        .expect("stepped pose remains finite")
        .expect("inserted cube remains present");

    assert!(matches!(outcome, SettlingOutcome::Settled { steps: 1, .. }));
    let z = pose.translation()[2].get();
    assert!(
        (0.18..0.20).contains(&z),
        "four substeps must advance one 0.1 s logical timestep, got z={z}"
    );
}

#[test]
fn stable_checks_before_minimum_steps_are_retained_until_settled() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 1\nsolver_iterations = 3\nmin_steps = 3\nmax_steps = 5\nstability_check_interval = 1\nlinear_sleep_threshold_mps = 100.0\nangular_sleep_threshold_rad_s = 100.0\nrequired_stable_checks = 2",
    );
    let mut world = populated_world(&plan);

    let outcome = world.settle(*plan.physics()).expect("world settles");

    assert_eq!(
        outcome,
        SettlingOutcome::Settled {
            steps: 3,
            consecutive_stable_checks: 3,
        }
    );
}

#[test]
fn moving_body_reaches_exact_maximum_with_its_current_counter() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 1\nsolver_iterations = 1\nmin_steps = 1\nmax_steps = 3\nstability_check_interval = 1\nlinear_sleep_threshold_mps = 0.0\nangular_sleep_threshold_rad_s = 0.0\nrequired_stable_checks = 1",
    );
    let mut world = populated_world(&plan);

    let outcome = world
        .settle(*plan.physics())
        .expect("world advances to its limit");

    assert_eq!(
        outcome,
        SettlingOutcome::MaximumStepsReached {
            steps: 3,
            consecutive_stable_checks: 0,
        }
    );
    assert!(!outcome.is_settled());
}

#[test]
fn stable_non_check_aligned_maximum_retains_only_completed_check_history() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 1\nsolver_iterations = 1\nmin_steps = 5\nmax_steps = 5\nstability_check_interval = 2\nlinear_sleep_threshold_mps = 0.0\nangular_sleep_threshold_rad_s = 0.0\nrequired_stable_checks = 2",
    );
    let mut world = PhysicsWorld::try_new(plan.bin()).expect("finite bin constructs");

    let outcome = world
        .settle(*plan.physics())
        .expect("empty world advances to its limit");

    assert_eq!(
        outcome,
        SettlingOutcome::MaximumStepsReached {
            steps: 5,
            consecutive_stable_checks: 2,
        }
    );
}

#[test]
fn empty_world_is_vacuously_stable_but_obeys_minimum_steps() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 1\nsolver_iterations = 1\nmin_steps = 2\nmax_steps = 4\nstability_check_interval = 1\nlinear_sleep_threshold_mps = 0.0\nangular_sleep_threshold_rad_s = 0.0\nrequired_stable_checks = 1",
    );
    let mut world = PhysicsWorld::try_new(plan.bin()).expect("finite bin constructs");

    let outcome = world.settle(*plan.physics()).expect("empty world advances");

    assert_eq!(
        outcome,
        SettlingOutcome::Settled {
            steps: 2,
            consecutive_stable_checks: 2,
        }
    );
}

#[test]
fn repeated_calls_start_a_fresh_stability_counter() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 1\nsolver_iterations = 1\nmin_steps = 1\nmax_steps = 1\nstability_check_interval = 1\nlinear_sleep_threshold_mps = 100.0\nangular_sleep_threshold_rad_s = 100.0\nrequired_stable_checks = 1",
    );
    let mut world = populated_world(&plan);

    let first = world.settle(*plan.physics()).expect("first call settles");
    let second = world.settle(*plan.physics()).expect("second call settles");

    assert_eq!(first.consecutive_stable_checks(), 1);
    assert_eq!(second.consecutive_stable_checks(), 1);
}

#[test]
fn identical_contacting_populations_replay_exact_outcomes_and_final_pose_bits() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 2\nsolver_iterations = 8\nmin_steps = 240\nmax_steps = 960\nstability_check_interval = 24\nlinear_sleep_threshold_mps = 0.05\nangular_sleep_threshold_rad_s = 0.05\nrequired_stable_checks = 10",
    );
    let mut first = populated_world_with_count(&plan, 4);
    let mut second = populated_world_with_count(&plan, 4);

    let first_outcome = first.settle(*plan.physics()).expect("first world settles");
    let second_outcome = second
        .settle(*plan.physics())
        .expect("second world settles");
    assert_eq!(first_outcome, second_outcome);
    assert!(first_outcome.is_settled());
    assert!(first_outcome.steps() >= 240);
    for id in 1..=4 {
        let id = InstanceId::new(id).expect("nonzero instance ID");
        let first_pose = first
            .body_pose(id)
            .expect("first pose is finite")
            .expect("first cube remains present");
        let second_pose = second
            .body_pose(id)
            .expect("second pose is finite")
            .expect("second cube remains present");
        assert_eq!(
            first_pose
                .translation()
                .map(|metres| metres.get().to_bits()),
            second_pose
                .translation()
                .map(|metres| metres.get().to_bits())
        );
        assert_eq!(
            first_pose.rotation_xyzw().map(f32::to_bits),
            second_pose.rotation_xyzw().map(f32::to_bits)
        );
    }
}

#[test]
fn falling_contact_population_settles_after_a_meaningful_window() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 2\nsolver_iterations = 8\nmin_steps = 240\nmax_steps = 960\nstability_check_interval = 24\nlinear_sleep_threshold_mps = 0.05\nangular_sleep_threshold_rad_s = 0.05\nrequired_stable_checks = 10",
    );
    let mut world = populated_world_with_count(&plan, 4);

    let outcome = world
        .settle(*plan.physics())
        .expect("falling bodies settle through contacts");

    assert!(outcome.is_settled());
    assert!(outcome.steps() >= 240);
    for id in 1..=4 {
        assert!(
            world
                .body_pose(InstanceId::new(id).expect("nonzero instance ID"))
                .expect("final pose is finite")
                .is_some()
        );
    }
}

fn measure_settling_only(label: &str, plan: &RunPlan, count: u16) {
    let mut worlds = (0..31)
        .map(|_| populated_world_with_count(plan, count))
        .collect::<Vec<_>>();
    let mut durations = Vec::with_capacity(worlds.len());
    let mut steps = Vec::with_capacity(worlds.len());
    for world in &mut worlds {
        let started = Instant::now();
        let outcome = world
            .settle(*plan.physics())
            .expect("checked world settles");
        durations.push(started.elapsed());
        assert!(outcome.is_settled());
        assert!(outcome.steps() >= plan.physics().min_steps());
        assert!(outcome.consecutive_stable_checks() >= plan.physics().required_stable_checks());
        steps.push(outcome.steps());
        for id in 1..=count {
            assert!(
                world
                    .body_pose(InstanceId::new(id).expect("nonzero instance ID"))
                    .expect("final pose is finite")
                    .is_some()
            );
        }
    }
    durations.sort_unstable();
    steps.sort_unstable();
    let total = durations.iter().copied().sum::<Duration>();
    println!(
        "synthetic settling-only {label}: objects={count}, median_steps={}, median={:?}, total={total:?}",
        steps[steps.len() / 2],
        durations[durations.len() / 2],
    );
}

#[test]
#[ignore = "manual release settling benchmark"]
fn measure_small_synthetic_settling_only() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 2\nsolver_iterations = 8\nmin_steps = 240\nmax_steps = 960\nstability_check_interval = 24\nlinear_sleep_threshold_mps = 0.05\nangular_sleep_threshold_rad_s = 0.05\nrequired_stable_checks = 10",
    );
    measure_settling_only("small", &plan, 4);
}

#[test]
#[ignore = "manual release settling benchmark"]
fn measure_medium_synthetic_settling_only() {
    let plan = plan(
        "fixed_timestep_s = 0.008333333\nsubsteps = 2\nsolver_iterations = 8\nmin_steps = 240\nmax_steps = 960\nstability_check_interval = 24\nlinear_sleep_threshold_mps = 0.05\nangular_sleep_threshold_rad_s = 0.05\nrequired_stable_checks = 10",
    );
    measure_settling_only("medium", &plan, 16);
}

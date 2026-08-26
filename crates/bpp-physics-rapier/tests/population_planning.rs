//! Public deterministic population-planning contracts.

use std::{sync::Arc, thread, time::Instant};

use bpp_core::RunConfig;
use bpp_physics_rapier::{PopulationPlanner, PopulationPlanningError};

const SEED: [u8; 32] = [0x5a; 32];

fn run_plan(object_count: &str, asset_mixture: &str, assets: &str) -> bpp_core::RunPlan {
    RunConfig::from_toml(&format!(
        r#"
[run]
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

{assets}

[population]
object_count = {object_count}
asset_mixture = {asset_mixture}
spawn_volume = {{ min_m = [-0.25, -0.2, 0.0], max_m = [0.25, 0.2, 0.25] }}

[physics]
substeps = 1
solver_iterations = 1

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
"#,
    ))
    .expect("test configuration parses")
    .into_plan()
}

fn assets(mass_a: &str, mass_b: &str, mixture_weight_a: &str, mixture_weight_b: &str) -> String {
    format!(
        r#"
[[assets]]
id = "alpha"
source_path = "alpha.glb"
source_unit_scale_m = 1.0
model_transform = {{ translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
mixture_weight = {mixture_weight_a}
physical_properties = {{ mass_kg = {mass_a}, friction = {{ kind = "uniform", min = 0.1, max = 0.9 }}, restitution = {{ kind = "uniform", min = 0.1, max = 0.9 }} }}
vhacd = {{ resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }}

[[assets]]
id = "bravo"
source_path = "bravo.glb"
source_unit_scale_m = 1.0
model_transform = {{ translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
mixture_weight = {mixture_weight_b}
physical_properties = {{ mass_kg = {mass_b}, friction = {{ kind = "uniform", min = 0.1, max = 0.9 }}, restitution = {{ kind = "uniform", min = 0.1, max = 0.9 }} }}
vhacd = {{ resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }}
"#,
    )
}

fn default_assets() -> String {
    assets(
        r#"{ kind = "uniform", min = 0.5, max = 2.5 }"#,
        r#"{ kind = "uniform", min = 3.5, max = 5.5 }"#,
        "1.0",
        "1.0",
    )
}

fn summary(plan: &bpp_physics_rapier::PopulationPlan) -> Vec<(u16, String, u32, u32, u32)> {
    plan.instances()
        .iter()
        .map(|instance| {
            let properties = instance.physical_properties();
            (
                instance.instance_id().get(),
                instance.asset_id().as_str().to_owned(),
                properties.mass_kg().to_bits(),
                properties.friction().to_bits(),
                properties.restitution().to_bits(),
            )
        })
        .collect()
}

#[test]
fn plans_a_known_protocol_vector_in_contiguous_identity_order() {
    let plan = run_plan(
        r#"{ kind = "fixed", value = 3 }"#,
        r#"{ kind = "choice", values = [{ value = "alpha", weight = 1.0 }, { value = "bravo", weight = 3.0 }] }"#,
        &default_assets(),
    );
    let planner = PopulationPlanner::try_new(&plan).expect("checked run plan compiles");

    let population = planner.plan(SEED).expect("population plans");

    assert_eq!(
        summary(&population),
        vec![
            (
                1,
                "bravo".to_owned(),
                1_081_405_984,
                1_061_181_091,
                1_058_814_228
            ),
            (
                2,
                "bravo".to_owned(),
                1_084_416_878,
                1_057_062_558,
                1_057_866_732
            ),
            (
                3,
                "bravo".to_owned(),
                1_085_124_884,
                1_051_621_243,
                1_060_727_954
            ),
        ]
    );
}

#[test]
fn same_seed_replays_exactly_and_asset_catalog_weights_do_not_participate() {
    let mixture = r#"{ kind = "choice", values = [{ value = "alpha", weight = 1.0 }, { value = "bravo", weight = 3.0 }] }"#;
    let first_plan = run_plan(
        r#"{ kind = "fixed", value = 12 }"#,
        mixture,
        &assets(
            r#"{ kind = "uniform", min = 0.5, max = 2.5 }"#,
            r#"{ kind = "uniform", min = 3.5, max = 5.5 }"#,
            "0.0",
            "1e308",
        ),
    );
    let second_plan = run_plan(
        r#"{ kind = "fixed", value = 12 }"#,
        mixture,
        &assets(
            r#"{ kind = "uniform", min = 0.5, max = 2.5 }"#,
            r#"{ kind = "uniform", min = 3.5, max = 5.5 }"#,
            "1e308",
            "0.0",
        ),
    );
    let first = PopulationPlanner::try_new(&first_plan).expect("first plan compiles");
    let second = PopulationPlanner::try_new(&second_plan).expect("second plan compiles");

    let replayed = first.plan(SEED).expect("first population plans");
    assert_eq!(
        summary(&replayed),
        summary(&first.plan(SEED).expect("replay plans"))
    );
    assert_eq!(
        summary(&replayed),
        summary(&second.plan(SEED).expect("second plans"))
    );
}

#[test]
fn changing_mass_distribution_does_not_change_count_asset_or_other_properties() {
    let mixture = r#"{ kind = "choice", values = [{ value = "alpha", weight = 1.0 }, { value = "bravo", weight = 1.0 }] }"#;
    let baseline = run_plan(
        r#"{ kind = "uniform", min = 4, max = 4 }"#,
        mixture,
        &default_assets(),
    );
    let changed_mass = run_plan(
        r#"{ kind = "uniform", min = 4, max = 4 }"#,
        mixture,
        &assets(
            r#"{ kind = "fixed", value = 9.0 }"#,
            r#"{ kind = "fixed", value = 10.0 }"#,
            "1.0",
            "1.0",
        ),
    );

    let baseline = PopulationPlanner::try_new(&baseline)
        .expect("baseline compiles")
        .plan(SEED)
        .expect("baseline plans");
    let changed_mass = PopulationPlanner::try_new(&changed_mass)
        .expect("changed plan compiles")
        .plan(SEED)
        .expect("changed plan plans");

    for (baseline, changed) in baseline.instances().iter().zip(changed_mass.instances()) {
        assert_eq!(baseline.instance_id(), changed.instance_id());
        assert_eq!(baseline.asset_id(), changed.asset_id());
        assert_eq!(
            baseline.physical_properties().friction().to_bits(),
            changed.physical_properties().friction().to_bits()
        );
        assert_eq!(
            baseline.physical_properties().restitution().to_bits(),
            changed.physical_properties().restitution().to_bits()
        );
    }
}

#[test]
fn samples_uniform_float_bounds_without_selecting_the_exclusive_upper_bound() {
    let plan = run_plan(
        r#"{ kind = "fixed", value = 128 }"#,
        r#"{ kind = "fixed", value = "alpha" }"#,
        &assets(
            r#"{ kind = "uniform", min = 1.0, max = 2.0 }"#,
            r#"{ kind = "fixed", value = 3.0 }"#,
            "1.0",
            "1.0",
        ),
    );
    let population = PopulationPlanner::try_new(&plan)
        .expect("plan compiles")
        .plan(SEED)
        .expect("population plans");

    for instance in population.instances() {
        let properties = instance.physical_properties();
        assert!((1.0..2.0).contains(&properties.mass_kg()));
        assert!((0.1..0.9).contains(&properties.friction()));
        assert!((0.1..0.9).contains(&properties.restitution()));
    }
}

#[test]
fn rejects_65536_instances_before_allocating_and_accepts_65535() {
    let assets = default_assets();
    let maximum = run_plan(
        r#"{ kind = "fixed", value = 65535 }"#,
        r#"{ kind = "fixed", value = "alpha" }"#,
        &assets,
    );
    let excessive = run_plan(
        r#"{ kind = "fixed", value = 65536 }"#,
        r#"{ kind = "fixed", value = "alpha" }"#,
        &assets,
    );

    let population = PopulationPlanner::try_new(&maximum)
        .expect("maximum plan compiles")
        .plan(SEED)
        .expect("maximum count plans");
    assert_eq!(population.instances().len(), usize::from(u16::MAX));
    assert_eq!(
        population
            .instances()
            .last()
            .expect("nonempty")
            .instance_id()
            .get(),
        u16::MAX
    );

    let error = PopulationPlanner::try_new(&excessive)
        .expect("excessive plan still compiles")
        .plan(SEED)
        .expect_err("excessive count must be rejected");
    assert!(matches!(
        error,
        PopulationPlanningError::ObjectCountExceedsInstanceIdCapacity { count: 65_536 }
    ));
}

#[test]
fn one_planner_returns_the_same_result_across_thread_schedules() {
    let plan = run_plan(
        r#"{ kind = "fixed", value = 32 }"#,
        r#"{ kind = "choice", values = [{ value = "alpha", weight = 1.0 }, { value = "bravo", weight = 1e308 }] }"#,
        &default_assets(),
    );
    let planner = Arc::new(PopulationPlanner::try_new(&plan).expect("plan compiles"));
    let expected = summary(&planner.plan(SEED).expect("baseline plans"));
    let mut threads = Vec::new();
    for index in 0_u8..8 {
        let planner = Arc::clone(&planner);
        threads.push(thread::spawn(move || {
            let mut seed = SEED;
            seed[0] = index;
            let _ = planner.plan(seed).expect("other seed plans");
            summary(&planner.plan(SEED).expect("shared seed plans"))
        }));
    }
    for thread in threads {
        assert_eq!(thread.join().expect("thread does not panic"), expected);
    }
}

#[test]
#[ignore = "manual release planning benchmark"]
fn measure_100_object_population_planning() {
    let plan = run_plan(
        r#"{ kind = "fixed", value = 100 }"#,
        r#"{ kind = "choice", values = [{ value = "alpha", weight = 1.0 }, { value = "bravo", weight = 3.0 }] }"#,
        &default_assets(),
    );
    let planner =
        PopulationPlanner::try_new(&plan).expect("planner construction is outside timing");
    let mut samples = Vec::with_capacity(31);
    for index in 0_u8..31 {
        let mut seed = SEED;
        seed[0] = index;
        let started = Instant::now();
        let population = planner.plan(seed).expect("population plans");
        samples.push(started.elapsed());
        assert_eq!(population.instances().len(), 100);
    }
    samples.sort_unstable();
    eprintln!(
        "100-object population-planning median: {:?}",
        samples[samples.len() / 2]
    );
}

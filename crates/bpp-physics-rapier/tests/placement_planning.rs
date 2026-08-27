//! Public deterministic placement-planning contracts.

use std::{collections::BTreeMap, sync::Arc, thread, time::Instant};

use bpp_assets::ConvexPart;
use bpp_core::{AssetId, RunConfig};
use bpp_physics_rapier::{
    PhysicsWorld, PlacementPlanner, PlacementPlanningError, PopulationPlanner, PreparedPhysicsAsset,
};

const POPULATION_SEED: [u8; 32] = [0x5a; 32];
const PLACEMENT_SEED: [u8; 32] = [0xa5; 32];
type PlacementSummary = (u16, String, [u32; 3], [u32; 3], [u32; 4]);

/// Catches removal of the public planner-to-body-spec handoff contract.
#[test]
fn places_a_population_with_stable_identity_and_borrowed_body_specs() {
    let plan = run_plan();
    let asset_id = AssetId::new("cube").expect("test ID is valid");
    let prepared_asset = Arc::new(
        PreparedPhysicsAsset::try_new(&[cube()]).expect("cube collision geometry prepares"),
    );
    let population = PopulationPlanner::try_new(&plan)
        .expect("checked run plan compiles")
        .plan(POPULATION_SEED)
        .expect("population plans");
    let planner = PlacementPlanner::try_new(
        &plan,
        BTreeMap::from([(asset_id.clone(), Arc::clone(&prepared_asset))]),
    )
    .expect("prepared catalog covers the selectable population asset");

    let placement = planner
        .place(&population, PLACEMENT_SEED)
        .expect("one cube fits in the spawn volume");

    assert_eq!(placement.instances().len(), 1);
    assert_eq!(
        placement.instances()[0].instance_id(),
        population.instances()[0].instance_id()
    );
    assert_eq!(placement.instances()[0].asset_id(), &asset_id);
    let mut bodies = placement.dynamic_body_specs();
    let body = bodies.next().expect("one body spec is exposed");
    assert_eq!(body.instance_id(), population.instances()[0].instance_id());
    assert_eq!(body.asset_id(), &asset_id);
    assert!(std::ptr::eq(
        body.prepared_asset(),
        Arc::as_ref(&prepared_asset)
    ));
    assert_eq!(bodies.len(), 0);

    let mut world = PhysicsWorld::try_new(plan.bin()).expect("fresh world constructs");
    world
        .insert_dynamic_bodies(&placement.dynamic_body_specs().collect::<Vec<_>>())
        .expect("planner body specs insert directly into a fresh world");
    assert_eq!(world.dynamic_body_count(), 1);
}

fn run_plan() -> bpp_core::RunPlan {
    run_plan_with(
        1,
        "[-0.4, -0.4, 0.0]",
        "[0.4, 0.4, 0.8]",
        "[0.0, 0.0, 0.0]",
        "[0.0, 0.0, 0.0, 1.0]",
        64,
    )
}

fn run_plan_with(
    object_count: u32,
    spawn_min: &str,
    spawn_max: &str,
    bin_translation: &str,
    bin_rotation: &str,
    attempts: u32,
) -> bpp_core::RunPlan {
    let source = r#"
[run]
master_seed = 1
valid_scene_count = 1
placement_attempt_limit_per_object = $ATTEMPTS
simulation_workers = 1
renderer_workers = 1
queue_capacity = 1
output_path = "output"
cache_path = "cache"

[bin]
inner_dimensions_m = [1.0, 1.0, 1.0]
floor_thickness_m = 0.02
wall_thickness_m = 0.03
world_from_bin = { translation = $BIN_TRANSLATION, rotation_xyzw = $BIN_ROTATION }
material_label = "steel"
physical_properties = { friction = 0.5, restitution = 0.1 }

[[assets]]
id = "cube"
source_path = "cube.glb"
source_unit_scale_m = 1.0
model_transform = { translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
mixture_weight = 1.0
physical_properties = { mass_kg = { kind = "fixed", value = 1.0 }, friction = { kind = "fixed", value = 0.5 }, restitution = { kind = "fixed", value = 0.1 } }
vhacd = { resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }

[population]
object_count = { kind = "fixed", value = $OBJECT_COUNT }
asset_mixture = { kind = "fixed", value = "cube" }
spawn_volume = { min_m = $SPAWN_MIN, max_m = $SPAWN_MAX }

[physics]
substeps = 1
solver_iterations = 1

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
    .replace("$ATTEMPTS", &attempts.to_string())
    .replace("$BIN_TRANSLATION", bin_translation)
    .replace("$BIN_ROTATION", bin_rotation)
    .replace("$OBJECT_COUNT", &object_count.to_string())
    .replace("$SPAWN_MIN", spawn_min)
    .replace("$SPAWN_MAX", spawn_max);
    RunConfig::from_toml(&source)
        .expect("test configuration parses")
        .into_plan()
}

fn catalog() -> (
    AssetId,
    Arc<PreparedPhysicsAsset>,
    BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
) {
    let asset_id = AssetId::new("cube").expect("test ID is valid");
    let prepared_asset = Arc::new(
        PreparedPhysicsAsset::try_new(&[cube()]).expect("cube collision geometry prepares"),
    );
    let catalog = BTreeMap::from([(asset_id.clone(), Arc::clone(&prepared_asset))]);
    (asset_id, prepared_asset, catalog)
}

fn summary(placement: &bpp_physics_rapier::PlacementPlan) -> Vec<PlacementSummary> {
    placement
        .instances()
        .iter()
        .map(|instance| {
            let properties = instance.physical_properties();
            (
                instance.instance_id().get(),
                instance.asset_id().as_str().to_owned(),
                [
                    properties.mass_kg().to_bits(),
                    properties.friction().to_bits(),
                    properties.restitution().to_bits(),
                ],
                instance
                    .world_from_model()
                    .translation()
                    .map(bpp_core::Meters::get)
                    .map(f32::to_bits),
                instance
                    .world_from_model()
                    .rotation_xyzw()
                    .map(f32::to_bits),
            )
        })
        .collect()
}

/// Catches protocol or pose-address drift at the public placement boundary.
#[test]
fn places_two_objects_with_the_locked_public_pose_vector() {
    let plan = run_plan_with(
        2,
        "[-0.4, -0.4, 0.0]",
        "[0.4, 0.4, 0.8]",
        "[0.0, 0.0, 0.0]",
        "[0.0, 0.0, 0.0, 1.0]",
        64,
    );
    let (_, _, catalog) = catalog();
    let population = PopulationPlanner::try_new(&plan)
        .expect("checked plan compiles")
        .plan(POPULATION_SEED)
        .expect("population plans");
    let placement = PlacementPlanner::try_new(&plan, catalog)
        .expect("catalog compiles")
        .place(&population, PLACEMENT_SEED)
        .expect("placement succeeds");

    assert_eq!(
        summary(&placement),
        vec![
            (
                1,
                "cube".to_owned(),
                [1_065_353_216, 1_056_964_608, 1_036_831_949],
                [3_195_204_706, 3_196_072_263, 1_045_427_082],
                [1_058_688_376, 1_056_521_893, 3_183_034_872, 1_059_073_983],
            ),
            (
                2,
                "cube".to_owned(),
                [1_065_353_216, 1_056_964_608, 1_036_831_949],
                [3_183_076_570, 3_192_611_492, 1_059_070_690],
                [1_043_544_074, 1_056_401_282, 1_062_642_263, 1_043_977_911],
            ),
        ]
    );
}

/// Catches mutable-RNG coupling, loss of replay, and shared-planner races.
#[test]
fn replays_exactly_and_is_schedule_independent() {
    let plan = run_plan_with(
        3,
        "[-0.4, -0.4, 0.0]",
        "[0.4, 0.4, 0.8]",
        "[0.0, 0.0, 0.0]",
        "[0.0, 0.0, 0.0, 1.0]",
        64,
    );
    let (_, _, catalog) = catalog();
    let population = PopulationPlanner::try_new(&plan)
        .expect("checked plan compiles")
        .plan(POPULATION_SEED)
        .expect("population plans");
    let planner = Arc::new(PlacementPlanner::try_new(&plan, catalog).expect("catalog compiles"));
    let expected = summary(
        &planner
            .place(&population, PLACEMENT_SEED)
            .expect("baseline places"),
    );
    assert_eq!(
        expected,
        summary(
            &planner
                .place(&population, PLACEMENT_SEED)
                .expect("same seed replays")
        )
    );
    let mut different_seed = PLACEMENT_SEED;
    different_seed[0] ^= 1;
    assert_ne!(
        expected,
        summary(
            &planner
                .place(&population, different_seed)
                .expect("other address places")
        )
    );

    let mut threads = Vec::new();
    for index in 0..8_u8 {
        let planner = Arc::clone(&planner);
        let population = population.clone();
        threads.push(thread::spawn(move || {
            let mut other = PLACEMENT_SEED;
            other[0] = index;
            let _ = planner
                .place(&population, other)
                .expect("other placement succeeds");
            summary(
                &planner
                    .place(&population, PLACEMENT_SEED)
                    .expect("shared seed succeeds"),
            )
        }));
    }
    for thread in threads {
        assert_eq!(thread.join().expect("thread does not panic"), expected);
    }
}

/// Catches accidentally applying the bin transform zero or multiple times.
#[test]
fn composes_the_bin_transform_exactly_once() {
    let plan = run_plan_with(
        1,
        "[-0.4, -0.4, 0.0]",
        "[0.4, 0.4, 0.8]",
        "[3.0, -2.0, 1.0]",
        "[0.0, 0.0, 0.70710677, 0.70710677]",
        64,
    );
    let reference_plan = run_plan_with(
        1,
        "[-0.4, -0.4, 0.0]",
        "[0.4, 0.4, 0.8]",
        "[0.0, 0.0, 0.0]",
        "[0.0, 0.0, 0.0, 1.0]",
        64,
    );
    let (_, _, catalog) = catalog();
    let population = PopulationPlanner::try_new(&plan)
        .expect("plan compiles")
        .plan(POPULATION_SEED)
        .expect("population plans");
    let placement = PlacementPlanner::try_new(&plan, catalog.clone())
        .expect("catalog compiles")
        .place(&population, PLACEMENT_SEED)
        .expect("placement succeeds");
    let identity_placement = PlacementPlanner::try_new(&reference_plan, catalog)
        .expect("reference catalog compiles")
        .place(&population, PLACEMENT_SEED)
        .expect("identity-bin placement succeeds");
    let expected = plan
        .bin()
        .world_from_bin()
        .compose(identity_placement.instances()[0].world_from_model())
        .expect("checked transform composition is finite");
    assert_eq!(placement.instances()[0].world_from_model(), expected,);
}

/// Catches accepting an overlap or exposing a partial plan after exhaustion.
#[test]
fn rejects_saturated_second_object_with_the_exact_attempt_count() {
    let plan = run_plan_with(
        2,
        "[-0.09, -0.09, 0.0]",
        "[0.09, 0.09, 0.18]",
        "[0.0, 0.0, 0.0]",
        "[0.0, 0.0, 0.0, 1.0]",
        7,
    );
    let (_, _, catalog) = catalog();
    let population = PopulationPlanner::try_new(&plan)
        .expect("plan compiles")
        .plan(POPULATION_SEED)
        .expect("population plans");
    let Err(error) = PlacementPlanner::try_new(&plan, catalog)
        .expect("catalog compiles")
        .place(&population, PLACEMENT_SEED)
    else {
        panic!("a second cube cannot obtain a strict AABB gap");
    };
    assert!(matches!(
        error,
        PlacementPlanningError::SpawnSaturated { instance_id, attempts: 7 }
            if instance_id == population.instances()[1].instance_id()
    ));
}

/// Catches accepting a selectable population asset without collision geometry.
#[test]
fn rejects_a_missing_selectable_prepared_asset() {
    let plan = run_plan();
    let Err(error) = PlacementPlanner::try_new(&plan, BTreeMap::new()) else {
        panic!("selectable cube requires a prepared collision asset");
    };
    assert!(
        matches!(error, PlacementPlanningError::MissingPreparedAsset { asset_id } if asset_id.as_str() == "cube")
    );
}

/// Catches deriving a center from unrotated geometry or skipping final AABB containment.
#[test]
fn keeps_every_rotated_elongated_collision_corner_inside_the_spawn_volume() {
    let plan = run_plan_with(
        1,
        "[-0.4, -0.4, 0.0]",
        "[0.4, 0.4, 0.8]",
        "[0.0, 0.0, 0.0]",
        "[0.0, 0.0, 0.0, 1.0]",
        64,
    );
    let (asset_id, _, _) = catalog();
    let elongated = Arc::new(
        PreparedPhysicsAsset::try_new(&[box_part([0.19, 0.025, 0.025])])
            .expect("elongated collision geometry prepares"),
    );
    let population = PopulationPlanner::try_new(&plan)
        .expect("plan compiles")
        .plan(POPULATION_SEED)
        .expect("population plans");
    let placement = PlacementPlanner::try_new(&plan, BTreeMap::from([(asset_id, elongated)]))
        .expect("catalog compiles")
        .place(&population, PLACEMENT_SEED)
        .expect("rotated elongated box fits");
    let pose = placement.instances()[0].world_from_model();
    for point in box_corners([0.19, 0.025, 0.025]) {
        let transformed = pose
            .transform_point(point.map(|value| bpp_core::Meters::new(value).expect("finite point")))
            .expect("finite transformed corner")
            .map(bpp_core::Meters::get);
        assert!((-0.4..=0.4).contains(&transformed[0]));
        assert!((-0.4..=0.4).contains(&transformed[1]));
        assert!((0.0..=0.8).contains(&transformed[2]));
    }
}

#[test]
#[ignore = "manual release placement benchmarks"]
fn measure_100_object_planning_only() {
    const SAMPLES: usize = 31;
    let (planner, population) = success_benchmark_fixture();
    let mut samples = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        let mut seed = PLACEMENT_SEED;
        seed[0] = u8::try_from(sample).expect("sample fits u8");
        let started = Instant::now();
        let result = planner.place(&population, seed);
        samples.push(started.elapsed());
        assert_eq!(
            result.expect("low-density plan succeeds").instances().len(),
            100
        );
    }
    samples.sort_unstable();
    eprintln!(
        "100-object planning-only median: {:?}",
        samples[SAMPLES / 2]
    );
}

#[test]
#[ignore = "manual release placement benchmarks"]
fn measure_saturation_only() {
    const SAMPLES: usize = 31;
    let (planner, population) = saturation_benchmark_fixture();
    let mut samples = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        let mut seed = PLACEMENT_SEED;
        seed[0] = u8::try_from(sample).expect("sample fits u8");
        let started = Instant::now();
        let result = planner.place(&population, seed);
        samples.push(started.elapsed());
        assert!(matches!(
            result,
            Err(PlacementPlanningError::SpawnSaturated { attempts: 7, .. })
        ));
    }
    samples.sort_unstable();
    eprintln!("saturation-only median: {:?}", samples[SAMPLES / 2]);
}

fn success_benchmark_fixture() -> (PlacementPlanner, bpp_physics_rapier::PopulationPlan) {
    let plan = run_plan_with(
        100,
        "[-0.49, -0.49, 0.0]",
        "[0.49, 0.49, 0.98]",
        "[0.0, 0.0, 0.0]",
        "[0.0, 0.0, 0.0, 1.0]",
        128,
    );
    let asset_id = AssetId::new("cube").expect("test ID is valid");
    let small_asset = Arc::new(
        PreparedPhysicsAsset::try_new(&[box_part([0.01, 0.01, 0.01])])
            .expect("small benchmark collision geometry prepares"),
    );
    let population = PopulationPlanner::try_new(&plan)
        .expect("plan compiles")
        .plan(POPULATION_SEED)
        .expect("population plans");
    let planner = PlacementPlanner::try_new(&plan, BTreeMap::from([(asset_id, small_asset)]))
        .expect("success catalog compiles");
    (planner, population)
}

fn saturation_benchmark_fixture() -> (PlacementPlanner, bpp_physics_rapier::PopulationPlan) {
    let plan = run_plan_with(
        2,
        "[-0.09, -0.09, 0.0]",
        "[0.09, 0.09, 0.18]",
        "[0.0, 0.0, 0.0]",
        "[0.0, 0.0, 0.0, 1.0]",
        7,
    );
    let (_, _, catalog) = catalog();
    let population = PopulationPlanner::try_new(&plan)
        .expect("plan compiles")
        .plan(POPULATION_SEED)
        .expect("population plans");
    let planner = PlacementPlanner::try_new(&plan, catalog).expect("saturation catalog compiles");
    (planner, population)
}

fn cube() -> ConvexPart {
    box_part([0.05, 0.05, 0.05])
}

fn box_part(half_extents: [f32; 3]) -> ConvexPart {
    let [x, y, z] = half_extents;
    ConvexPart::try_new(
        vec![
            [-x, -y, -z],
            [x, -y, -z],
            [-x, y, -z],
            [x, y, -z],
            [-x, -y, z],
            [x, -y, z],
            [-x, y, z],
            [x, y, z],
        ],
        vec![
            0, 2, 1, 1, 2, 3, 4, 5, 6, 5, 7, 6, 0, 1, 4, 1, 5, 4, 2, 6, 3, 3, 6, 7, 0, 4, 2, 2, 4,
            6, 1, 3, 5, 3, 7, 5,
        ],
    )
    .expect("cube is a checked convex surface")
}

fn box_corners(half_extents: [f32; 3]) -> [[f32; 3]; 8] {
    let [x, y, z] = half_extents;
    [
        [-x, -y, -z],
        [x, -y, -z],
        [-x, y, -z],
        [x, y, -z],
        [-x, -y, z],
        [x, -y, z],
        [-x, y, z],
        [x, y, z],
    ]
}

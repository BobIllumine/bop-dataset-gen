//! Public dynamic-body insertion contracts for the Rapier physics world.

use bpp_assets::ConvexPart;
use bpp_core::{AssetId, InstanceId, ResolvedPhysicalProperties, RigidTransform, RunConfig};
use bpp_physics_rapier::{DynamicBodySpec, PhysicsWorld, PhysicsWorldError, PreparedPhysicsAsset};
use std::time::Instant;

/// Tolerance for finite `f32` values that round-trip through Rapier.
const RAPIER_F32_TOLERANCE: f32 = 1.0e-5;

fn bin() -> bpp_core::BinConfig {
    RunConfig::from_toml(
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
world_from_bin = { translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
material_label = "steel"
physical_properties = { friction = 0.5, restitution = 0.1 }

[[assets]]
id = "part"
source_path = "part.glb"
source_unit_scale_m = 1.0
model_transform = { translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
mixture_weight = 1.0
physical_properties = { mass_kg = { kind = "fixed", value = 0.5 }, friction = { kind = "fixed", value = 0.5 }, restitution = { kind = "fixed", value = 0.1 } }
vhacd = { resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }

[population]
object_count = { kind = "fixed", value = 1 }
asset_mixture = { kind = "fixed", value = "part" }
spawn_volume = { min_m = [-0.25, -0.2, 0.0], max_m = [0.25, 0.2, 0.25] }

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
"#,
    )
    .expect("test configuration parses")
    .into_plan()
    .bin()
    .clone()
}

fn asset_id() -> AssetId {
    AssetId::new("part").expect("nonempty asset ID")
}

fn instance_id(value: u16) -> InstanceId {
    InstanceId::new(value).expect("nonzero instance ID")
}

fn properties() -> ResolvedPhysicalProperties {
    ResolvedPhysicalProperties::try_new(2.5, 0.73, 0.27).expect("valid properties")
}

fn tetrahedron() -> ConvexPart {
    ConvexPart::try_new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        vec![0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3],
    )
    .expect("tetrahedron is checked")
}

fn identity_pose() -> RigidTransform {
    RigidTransform::try_new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]).expect("identity is checked")
}

fn spec<'a>(
    id: InstanceId,
    asset_id: &'a AssetId,
    asset: &'a PreparedPhysicsAsset,
) -> DynamicBodySpec<'a> {
    DynamicBodySpec::new(id, asset_id, asset, identity_pose(), properties())
}

fn assert_unchanged(world: &PhysicsWorld, expected_ids: &[InstanceId]) {
    assert_eq!(world.dynamic_body_count(), expected_ids.len());
    assert_eq!(world.instance_ids().collect::<Vec<_>>(), expected_ids);
    for &id in expected_ids {
        assert!(world.collider_count(id).is_some());
    }
}

#[test]
fn shuffled_input_inserts_and_iterates_by_ascending_instance_id() {
    let asset_id = asset_id();
    let parts = vec![tetrahedron()];
    let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
    let mut world = PhysicsWorld::try_new(&bin()).expect("finite bin constructs");
    let bodies = [
        spec(instance_id(3), &asset_id, &asset),
        spec(instance_id(1), &asset_id, &asset),
        spec(instance_id(2), &asset_id, &asset),
    ];

    world
        .insert_dynamic_bodies(&bodies)
        .expect("checked bodies insert");

    assert_eq!(
        world.instance_ids().collect::<Vec<_>>(),
        vec![instance_id(1), instance_id(2), instance_id(3)]
    );
}

#[test]
fn initial_pose_round_trips_within_documented_f32_tolerance() {
    let asset_id = asset_id();
    let parts = vec![tetrahedron()];
    let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
    let pose = RigidTransform::try_new(
        [1.25, -2.5, 3.75],
        [0.0, 0.0, 0.5_f32.sqrt(), 0.5_f32.sqrt()],
    )
    .expect("finite nontrivial pose is checked");
    let body = DynamicBodySpec::new(instance_id(1), &asset_id, &asset, pose, properties());
    let mut world = PhysicsWorld::try_new(&bin()).expect("finite bin constructs");

    world
        .insert_dynamic_bodies(&[body])
        .expect("checked body inserts");

    let actual = world
        .body_pose(instance_id(1))
        .expect("stored Rapier pose remains representable")
        .expect("inserted body has a pose");
    for (actual, expected) in actual
        .translation()
        .map(bpp_core::Meters::get)
        .into_iter()
        .zip(pose.translation().map(bpp_core::Meters::get))
        .chain(actual.rotation_xyzw().into_iter().zip(pose.rotation_xyzw()))
    {
        assert!((actual - expected).abs() <= RAPIER_F32_TOLERANCE);
    }
}

#[test]
fn one_body_with_two_convex_parts_reports_two_colliders() {
    let asset_id = asset_id();
    let parts = vec![tetrahedron(), tetrahedron()];
    let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
    let mut world = PhysicsWorld::try_new(&bin()).expect("finite bin constructs");

    world
        .insert_dynamic_bodies(&[spec(instance_id(1), &asset_id, &asset)])
        .expect("checked body inserts");

    assert_eq!(world.collider_count(instance_id(1)), Some(2));
}

#[test]
fn duplicate_ids_inside_a_batch_fail_atomically() {
    let asset_id = asset_id();
    let parts = vec![tetrahedron()];
    let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
    let mut world = PhysicsWorld::try_new(&bin()).expect("finite bin constructs");
    let bodies = [
        spec(instance_id(1), &asset_id, &asset),
        spec(instance_id(1), &asset_id, &asset),
    ];

    let error = world
        .insert_dynamic_bodies(&bodies)
        .expect_err("duplicate IDs must be rejected");

    assert!(matches!(
        error,
        PhysicsWorldError::DuplicateInstanceId { instance_id: id } if id == instance_id(1)
    ));
    assert_unchanged(&world, &[]);
}

#[test]
fn existing_id_fails_atomically() {
    let asset_id = asset_id();
    let parts = vec![tetrahedron()];
    let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
    let mut world = PhysicsWorld::try_new(&bin()).expect("finite bin constructs");
    world
        .insert_dynamic_bodies(&[spec(instance_id(1), &asset_id, &asset)])
        .expect("first body inserts");
    let bodies = [
        spec(instance_id(2), &asset_id, &asset),
        spec(instance_id(1), &asset_id, &asset),
    ];

    let error = world
        .insert_dynamic_bodies(&bodies)
        .expect_err("existing ID must be rejected");

    assert!(matches!(
        error,
        PhysicsWorldError::DuplicateInstanceId { instance_id: id } if id == instance_id(1)
    ));
    assert_unchanged(&world, &[instance_id(1)]);
}

#[test]
fn later_batch_with_lower_new_id_fails_atomically() {
    let asset_id = asset_id();
    let parts = vec![tetrahedron()];
    let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
    let mut world = PhysicsWorld::try_new(&bin()).expect("finite bin constructs");
    world
        .insert_dynamic_bodies(&[spec(instance_id(5), &asset_id, &asset)])
        .expect("first batch inserts");

    let error = world
        .insert_dynamic_bodies(&[
            spec(instance_id(7), &asset_id, &asset),
            spec(instance_id(4), &asset_id, &asset),
        ])
        .expect_err("later IDs must preserve global insertion order");

    assert!(matches!(
        error,
        PhysicsWorldError::InstanceIdNotAfterPreviousMaximum {
            instance_id: id,
            previous_maximum,
        } if id == instance_id(4) && previous_maximum == instance_id(5)
    ));
    assert_unchanged(&world, &[instance_id(5)]);
}

#[test]
#[ignore = "manual non-gating release insertion timing"]
fn measure_repeated_two_part_prepared_asset_insertion() {
    const BODY_COUNT: u16 = 100;
    const SAMPLE_COUNT: usize = 25;

    let bin = bin();
    let asset_id = asset_id();
    let parts = vec![tetrahedron(), tetrahedron()];
    let vertex_count: usize = parts.iter().map(|part| part.vertices().len()).sum();
    let triangle_count: usize = parts.iter().map(|part| part.indices().len() / 3).sum();
    let preparation_start = Instant::now();
    let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
    let preparation_elapsed = preparation_start.elapsed();
    let bodies: Vec<_> = (1..=BODY_COUNT)
        .map(|id| spec(instance_id(id), &asset_id, &asset))
        .collect();

    let mut insertion_samples = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let mut world = PhysicsWorld::try_new(&bin).expect("finite bin constructs");
        let insertion_start = Instant::now();
        world
            .insert_dynamic_bodies(&bodies)
            .expect("representative body batch inserts");
        insertion_samples.push(insertion_start.elapsed());
        assert_eq!(world.dynamic_body_count(), usize::from(BODY_COUNT));
    }
    insertion_samples.sort_unstable();
    let median_insertion = insertion_samples[SAMPLE_COUNT / 2];

    eprintln!(
        "serial Rapier synthetic fixture: preparation={preparation_elapsed:?}, samples={SAMPLE_COUNT}, bodies={BODY_COUNT}, parts_per_body={}, vertices_per_asset={vertex_count}, triangles_per_asset={triangle_count}, median_insertion={median_insertion:?}",
        asset.part_count()
    );
}

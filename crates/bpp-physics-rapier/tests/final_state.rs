//! Public final-state validation contracts.

use bpp_assets::ConvexPart;
use bpp_core::{
    AssetId, InstanceId, PhysicsRejectionReason, ResolvedPhysicalProperties, RigidTransform,
    RunConfig, RunPlan,
};
use bpp_physics_rapier::{
    DynamicBodySpec, FinalStateValidationError, PhysicsWorld, PreparedPhysicsAsset,
};

fn plan(maximum_penetration_m: f32) -> RunPlan {
    plan_with_bin_pose(maximum_penetration_m, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0])
}

fn plan_with_bin_pose(
    maximum_penetration_m: f32,
    translation: [f32; 3],
    rotation_xyzw: [f32; 4],
) -> RunPlan {
    let [tx, ty, tz] = translation;
    let [rx, ry, rz, rw] = rotation_xyzw;
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
world_from_bin = {{ translation = [{tx}, {ty}, {tz}], rotation_xyzw = [{rx}, {ry}, {rz}, {rw}] }}
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
fixed_timestep_s = 0.004166667
substeps = 1
solver_iterations = 8
min_steps = 1
max_steps = 1
stability_check_interval = 1
linear_sleep_threshold_mps = 100.0
angular_sleep_threshold_rad_s = 100.0
required_stable_checks = 1
escape_margin_m = 0.002
maximum_penetration_m = {maximum_penetration_m}

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

fn world_with_poses(plan: &RunPlan, poses: &[[f32; 3]]) -> PhysicsWorld {
    let asset_id = AssetId::new("cube").expect("valid asset ID");
    let asset = PreparedPhysicsAsset::try_new(&[cube()]).expect("cube prepares");
    let properties = ResolvedPhysicalProperties::try_new(0.5, 0.5, 0.1).expect("properties");
    let mut world = PhysicsWorld::try_new(plan.bin()).expect("bin constructs");
    let bodies = poses
        .iter()
        .enumerate()
        .map(|(index, translation)| {
            DynamicBodySpec::new(
                InstanceId::new(u16::try_from(index + 1).expect("small fixture")).unwrap(),
                &asset_id,
                &asset,
                RigidTransform::try_new(*translation, [0.0, 0.0, 0.0, 1.0]).unwrap(),
                properties,
            )
        })
        .collect::<Vec<_>>();
    world.insert_dynamic_bodies(&bodies).expect("bodies insert");
    world
}

#[test]
fn finite_inside_body_returns_zero_initial_metrics() {
    let plan = plan(0.002);
    let world = world_with_poses(&plan, &[[0.0, 0.0, 0.25]]);

    let metrics = world
        .validate_final_state(plan.bin(), *plan.physics())
        .expect("inside finite body is valid");

    assert_eq!(metrics.max_linear_speed_mps().to_bits(), 0.0_f32.to_bits());
    assert_eq!(
        metrics.max_angular_speed_rad_s().to_bits(),
        0.0_f32.to_bits()
    );
    assert_eq!(metrics.max_penetration_m().to_bits(), 0.0_f32.to_bits());
}

#[test]
fn body_completely_beyond_outer_wall_is_escape() {
    let plan = plan(0.002);
    let world = world_with_poses(&plan, &[[2.0, 0.0, 0.25]]);

    let error = world
        .validate_final_state(plan.bin(), *plan.physics())
        .expect_err("body outside the guard prism must be rejected");

    assert!(
        matches!(error, FinalStateValidationError::Escaped { instance_id } if instance_id.get() == 1)
    );
    assert_eq!(error.rejection_reason(), PhysicsRejectionReason::Escape);
}

#[test]
fn object_partially_above_open_top_is_valid_but_fully_above_is_escape() {
    let plan = plan(0.002);
    let partially_above = world_with_poses(&plan, &[[0.0, 0.0, 0.54]]);
    partially_above
        .validate_final_state(plan.bin(), *plan.physics())
        .expect("an object intersecting the open bin prism remains legal");

    let fully_above = world_with_poses(&plan, &[[0.0, 0.0, 0.56]]);
    let error = fully_above
        .validate_final_state(plan.bin(), *plan.physics())
        .expect_err("an object completely above the guard prism escaped");
    assert!(matches!(error, FinalStateValidationError::Escaped { .. }));
}

#[test]
fn overlapping_bodies_are_rejected_for_strict_zero_penetration_limit() {
    let plan = plan(0.0);
    let mut world = world_with_poses(&plan, &[[0.0, 0.0, 0.25], [0.0, 0.0, 0.25]]);
    world
        .settle(*plan.physics())
        .expect("one step updates contact manifolds");

    let error = world
        .validate_final_state(plan.bin(), *plan.physics())
        .expect_err("overlap must exceed the strict zero limit");

    assert!(matches!(
        error,
        FinalStateValidationError::ExcessivePenetration {
            first: Some(first),
            second: Some(second),
            depth_m,
            limit_m,
        } if first.get() == 1 && second.get() == 2 && depth_m > 0.0 && limit_m == 0.0
    ));
}

#[test]
fn escape_guard_is_evaluated_in_a_rotated_translated_bin_frame() {
    let half = 0.5_f32.sqrt();
    let plan = plan_with_bin_pose(0.002, [10.0, -3.0, 2.0], [0.0, 0.0, half, half]);
    let inside = world_with_poses(&plan, &[[10.0, -3.0, 2.25]]);
    inside
        .validate_final_state(plan.bin(), *plan.physics())
        .expect("body at the transformed bin center is valid");

    let outside = world_with_poses(&plan, &[[10.0, -1.0, 2.25]]);
    assert!(matches!(
        outside.validate_final_state(plan.bin(), *plan.physics()),
        Err(FinalStateValidationError::Escaped { instance_id }) if instance_id.get() == 1
    ));
}

#[test]
fn penetration_rejection_precedes_escape_when_both_apply() {
    let plan = plan(0.0);
    let mut world = world_with_poses(&plan, &[[2.0, 0.0, 0.25], [2.0, 0.0, 0.25]]);
    world
        .settle(*plan.physics())
        .expect("one step updates contact manifolds");

    assert!(matches!(
        world.validate_final_state(plan.bin(), *plan.physics()),
        Err(FinalStateValidationError::ExcessivePenetration { .. })
    ));
}

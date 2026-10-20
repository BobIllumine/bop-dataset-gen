//! Public contracts for renderer-neutral analytic bin collision geometry.

use bop_assets::{AnalyticBinCollisionError, CollisionCuboid, analytic_bin_collision};
use bop_core::{RunConfig, RunPlan};

fn plan(inner: [f32; 3], wall: f32, world_translation: [f32; 3]) -> RunPlan {
    let [x, y, z] = inner;
    let [tx, ty, tz] = world_translation;
    let spawn_x = x / 4.0;
    let spawn_y = y / 4.0;
    let spawn_z = z / 2.0;
    let toml = format!(
        r#"[run]
master_seed = 1
valid_scene_count = 1
simulation_workers = 1
renderer_workers = 1
queue_capacity = 1
output_path = "output"
cache_path = "cache"

[bin]
inner_dimensions_m = [{x:e}, {y:e}, {z:e}]
floor_thickness_m = 0.02
wall_thickness_m = {wall:e}
world_from_bin = {{ translation = [{tx:e}, {ty:e}, {tz:e}], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
material_label = "steel"
physical_properties = {{ friction = 0.5, restitution = 0.1 }}

[[assets]]
id = "part"
source_path = "part.glb"
source_unit_scale_m = 1.0
model_transform = {{ translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
mixture_weight = 1.0
physical_properties = {{ mass_kg = {{ kind = "fixed", value = 0.5 }}, friction = {{ kind = "fixed", value = 0.5 }}, restitution = {{ kind = "fixed", value = 0.1 }} }}
vhacd = {{ resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }}

[population]
object_count = {{ kind = "fixed", value = 1 }}
asset_mixture = {{ kind = "fixed", value = "part" }}
spawn_volume = {{ min_m = [{neg_spawn_x:e}, {neg_spawn_y:e}, 0.0], max_m = [{spawn_x:e}, {spawn_y:e}, {spawn_z:e}] }}

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
        neg_spawn_x = -spawn_x,
        neg_spawn_y = -spawn_y,
    );
    RunConfig::from_toml(&toml)
        .expect("test config parses")
        .into_plan()
}

fn translation(cuboid: CollisionCuboid) -> [f32; 3] {
    cuboid
        .bin_from_cuboid()
        .translation()
        .map(bop_core::Meters::get)
}

fn extents(cuboid: CollisionCuboid) -> [f32; 3] {
    cuboid.half_extents_m().map(bop_core::PositiveMeters::get)
}

fn assert_bits(actual: [f32; 3], expected: [f32; 3]) {
    assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
}

#[test]
fn derives_the_five_named_cuboids_from_the_documented_bin_local_formulas() {
    let plan = plan([1.0, 0.8, 0.5], 0.03, [0.0, 0.0, 0.0]);
    let collision = analytic_bin_collision(plan.bin()).expect("finite bin derives");
    assert_bits(extents(collision.floor()), [0.53, 0.43, 0.01]);
    assert_bits(translation(collision.floor()), [0.0, 0.0, -0.01]);
    assert_bits(extents(collision.negative_x()), [0.015, 0.43, 0.25]);
    assert_bits(translation(collision.negative_x()), [-0.515, 0.0, 0.25]);
    assert_bits(extents(collision.positive_x()), [0.015, 0.43, 0.25]);
    assert_bits(translation(collision.positive_x()), [0.515, 0.0, 0.25]);
    assert_bits(extents(collision.negative_y()), [0.5, 0.015, 0.25]);
    assert_bits(translation(collision.negative_y()), [0.0, -0.415, 0.25]);
    assert_bits(extents(collision.positive_y()), [0.5, 0.015, 0.25]);
    assert_bits(translation(collision.positive_y()), [0.0, 0.415, 0.25]);

    assert_eq!(
        collision.cuboids(),
        [
            collision.floor(),
            collision.negative_x(),
            collision.positive_x(),
            collision.negative_y(),
            collision.positive_y(),
        ]
    );
}

#[test]
fn walls_meet_inner_faces_and_tile_outer_corners_without_intrusion() {
    let collision = analytic_bin_collision(plan([1.0, 0.8, 0.5], 0.03, [0.0; 3]).bin())
        .expect("finite bin derives");

    assert_bits(
        [
            translation(collision.negative_x())[0] + extents(collision.negative_x())[0],
            translation(collision.positive_x())[0] - extents(collision.positive_x())[0],
            translation(collision.negative_y())[1] + extents(collision.negative_y())[1],
        ],
        [-0.5, 0.5, -0.4],
    );
    assert_bits(
        [
            translation(collision.positive_y())[1] - extents(collision.positive_y())[1],
            extents(collision.floor())[0],
            extents(collision.floor())[1],
        ],
        [0.4, 0.53, 0.43],
    );
}

#[test]
fn local_collision_geometry_does_not_bake_in_world_from_bin() {
    let identity = plan([1.0, 0.8, 0.5], 0.03, [0.0; 3]);
    let translated = plan([1.0, 0.8, 0.5], 0.03, [10.0, -20.0, 30.0]);

    assert_eq!(
        analytic_bin_collision(identity.bin()),
        analytic_bin_collision(translated.bin())
    );
}

#[test]
fn derived_f32_overflow_is_a_typed_error() {
    let plan = plan([3.0e38, 2.0, 2.0], 3.0e38, [0.0; 3]);

    assert_eq!(
        analytic_bin_collision(plan.bin()),
        Err(AnalyticBinCollisionError::DerivedGeometryNotFinite)
    );
}

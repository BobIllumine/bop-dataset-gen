//! Public construction contracts for the analytic Rapier bin.

use bpp_core::RunConfig;
use bpp_physics_rapier::PhysicsWorld;

fn bin(
    translation: [f32; 3],
    rotation_xyzw: [f32; 4],
    friction: f32,
    restitution: f32,
) -> bpp_core::BinConfig {
    let [tx, ty, tz] = translation;
    let [rx, ry, rz, rw] = rotation_xyzw;
    let input = format!(
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
world_from_bin = {{ translation = [{tx:e}, {ty:e}, {tz:e}], rotation_xyzw = [{rx:e}, {ry:e}, {rz:e}, {rw:e}] }}
material_label = "steel"
physical_properties = {{ friction = {friction:e}, restitution = {restitution:e} }}

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
    );
    RunConfig::from_toml(&input)
        .expect("test configuration parses")
        .into_plan()
        .bin()
        .clone()
}

#[test]
fn constructs_identity_bin_with_exact_gravity_and_five_colliders() {
    let bin = bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1);

    let world = PhysicsWorld::try_new(&bin).expect("finite bin is constructible");

    assert_eq!(
        world.gravity_m_s2().map(f32::to_bits),
        [0.0, 0.0, -9.81].map(f32::to_bits)
    );
    assert_eq!(world.bin_collider_count(), 5);
}

#[test]
fn constructs_a_translated_and_rotated_bin() {
    let bin = bin(
        [10.0, -20.0, 30.0],
        [0.0, 0.0, 0.5_f32.sqrt(), 0.5_f32.sqrt()],
        0.5,
        0.1,
    );

    let world = PhysicsWorld::try_new(&bin).expect("world transform applies through fixed body");

    assert_eq!(
        world.gravity_m_s2().map(f32::to_bits),
        [0.0, 0.0, -9.81].map(f32::to_bits)
    );
    assert_eq!(world.bin_collider_count(), 5);
}

#[test]
fn constructs_a_bin_with_configured_contact_coefficients() {
    let bin = bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.73, 0.27);

    let world = PhysicsWorld::try_new(&bin).expect("configured contact coefficients are valid");

    assert_eq!(
        world.gravity_m_s2().map(f32::to_bits),
        [0.0, 0.0, -9.81].map(f32::to_bits)
    );
    assert_eq!(world.bin_collider_count(), 5);
}

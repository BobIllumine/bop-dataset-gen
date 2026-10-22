//! Public protocol contract for deterministic asset preprocessing cache keys.

use std::fmt::Write as _;

use bop_assets::{AssetCacheKeyInput, AssetSourceFormat, asset_cache_key};
use bop_core::{PositiveMeters, RigidTransform, RunConfig, VhacdParameters};

fn parameters(
    resolution: u32,
    maximum_hulls: u32,
    concavity: f32,
    plane_downsampling: u32,
    hull_downsampling: u32,
) -> VhacdParameters {
    let mut input = String::from(
        r#"[run]
master_seed = 1
valid_scene_count = 1
simulation_workers = 1
renderer_workers = 1
queue_capacity = 1
output_path = "output"
cache_path = "cache"
[bin]
inner_dimensions_m = [2.0, 2.0, 2.0]
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
"#,
    );
    writeln!(
        input,
        "vhacd = {{ resolution = {resolution}, max_convex_hulls = {maximum_hulls}, concavity = {concavity}, plane_downsampling = {plane_downsampling}, convex_hull_downsampling = {hull_downsampling} }}"
    )
    .expect("writing to a String succeeds");
    input.push_str(
        r#"[population]
object_count = { kind = "fixed", value = 1 }
asset_mixture = { kind = "fixed", value = "part" }
spawn_volume = { min_m = [-0.5, -0.5, 0.0], max_m = [0.5, 0.5, 1.0] }
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
    );
    RunConfig::from_toml(&input)
        .expect("cache-key parameter fixture is valid")
        .plan()
        .assets()[0]
        .vhacd()
}

fn key(
    source: &[u8],
    format: AssetSourceFormat,
    scale: f32,
    transform: RigidTransform,
    parameters: VhacdParameters,
) -> bop_assets::AssetCacheKey {
    asset_cache_key(AssetCacheKeyInput::new(
        source,
        format,
        PositiveMeters::try_new(scale).expect("test scale is positive"),
        transform,
        parameters,
    ))
}

#[test]
fn identical_inputs_reproduce_the_same_key_and_lowercase_hex() {
    let parameters = parameters(10_000, 16, 0.0025, 4, 4);
    let first = key(
        b"mesh bytes",
        AssetSourceFormat::Glb,
        1.0,
        RigidTransform::identity(),
        parameters,
    );
    let second = key(
        b"mesh bytes",
        AssetSourceFormat::Glb,
        1.0,
        RigidTransform::identity(),
        parameters,
    );

    assert_eq!(first, second);
    assert_eq!(first.as_bytes().len(), 32);
    let hex = first.to_hex();
    assert_eq!(hex.len(), 64);
    assert!(
        hex.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn source_bytes_format_and_units_affect_the_key() {
    let base_parameters = parameters(10_000, 16, 0.0025, 4, 4);
    let identity = RigidTransform::identity();
    let base = key(
        b"mesh bytes",
        AssetSourceFormat::Glb,
        1.0,
        identity,
        base_parameters,
    );

    let variants = [
        key(
            b"mesh byteS",
            AssetSourceFormat::Glb,
            1.0,
            identity,
            base_parameters,
        ),
        key(
            b"mesh bytes",
            AssetSourceFormat::Stl,
            1.0,
            identity,
            base_parameters,
        ),
        key(
            b"mesh bytes",
            AssetSourceFormat::Glb,
            0.001,
            identity,
            base_parameters,
        ),
    ];

    for variant in variants {
        assert_ne!(variant, base);
    }
}

#[test]
fn every_transform_axis_affects_the_key() {
    let base_parameters = parameters(10_000, 16, 0.0025, 4, 4);
    let base = key(
        b"mesh bytes",
        AssetSourceFormat::Glb,
        1.0,
        RigidTransform::identity(),
        base_parameters,
    );
    let transforms = [
        RigidTransform::try_new([0.25, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]),
        RigidTransform::try_new([0.0, 0.25, 0.0], [0.0, 0.0, 0.0, 1.0]),
        RigidTransform::try_new([0.0, 0.0, 0.25], [0.0, 0.0, 0.0, 1.0]),
        RigidTransform::try_new([0.0, 0.0, 0.0], [0.5, 0.0, 0.0, 0.5]),
        RigidTransform::try_new([0.0, 0.0, 0.0], [0.0, 0.5, 0.0, 0.5]),
        RigidTransform::try_new([0.0, 0.0, 0.0], [0.0, 0.0, 0.5, 0.5]),
    ];

    for transform in transforms {
        let variant = key(
            b"mesh bytes",
            AssetSourceFormat::Glb,
            1.0,
            transform.expect("test transform is valid"),
            base_parameters,
        );
        assert_ne!(variant, base);
    }
}

#[test]
fn every_vhacd_parameter_affects_the_key() {
    let identity = RigidTransform::identity();
    let base = key(
        b"mesh bytes",
        AssetSourceFormat::Glb,
        1.0,
        identity,
        parameters(10_000, 16, 0.0025, 4, 4),
    );
    let variants = [
        key(
            b"mesh bytes",
            AssetSourceFormat::Glb,
            1.0,
            identity,
            parameters(10_001, 16, 0.0025, 4, 4),
        ),
        key(
            b"mesh bytes",
            AssetSourceFormat::Glb,
            1.0,
            identity,
            parameters(10_000, 15, 0.0025, 4, 4),
        ),
        key(
            b"mesh bytes",
            AssetSourceFormat::Glb,
            1.0,
            identity,
            parameters(10_000, 16, 0.003, 4, 4),
        ),
        key(
            b"mesh bytes",
            AssetSourceFormat::Glb,
            1.0,
            identity,
            parameters(10_000, 16, 0.0025, 3, 4),
        ),
        key(
            b"mesh bytes",
            AssetSourceFormat::Glb,
            1.0,
            identity,
            parameters(10_000, 16, 0.0025, 4, 3),
        ),
    ];

    for variant in variants {
        assert_ne!(variant, base);
    }
}

#[test]
fn canonical_equivalent_transforms_share_a_key() {
    let parameters = parameters(10_000, 16, 0.0025, 4, 4);
    let positive = RigidTransform::try_new(
        [0.0, -0.0, 0.0],
        [
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ],
    )
    .expect("test transform is valid");
    let negative = RigidTransform::try_new(
        [-0.0, 0.0, -0.0],
        [
            0.0,
            0.0,
            -std::f32::consts::FRAC_1_SQRT_2,
            -std::f32::consts::FRAC_1_SQRT_2,
        ],
    )
    .expect("test transform is valid");

    assert_eq!(
        key(b"mesh", AssetSourceFormat::Stl, 0.001, positive, parameters),
        key(b"mesh", AssetSourceFormat::Stl, 0.001, negative, parameters),
    );
}

#[test]
fn v1_protocol_matches_the_locked_known_vector() {
    let digest = key(
        b"known cache fixture\0\xff",
        AssetSourceFormat::Stl,
        0.001,
        RigidTransform::try_new(
            [0.25, -0.5, 1.5],
            [
                0.0,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
        )
        .expect("known-vector transform is valid"),
        parameters(12_345, 7, 0.00125, 3, 5),
    );

    // The key includes this crate's version, so every release changes it.
    assert_eq!(
        digest.to_hex(),
        "59c6382c8b9af865508272d3315396a9aec9058450026827f921d6d8fce6cab9"
    );
}

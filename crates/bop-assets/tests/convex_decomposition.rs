//! Public contracts for deterministic, bounded convex decomposition.

use std::fmt::Write as _;

use bop_assets::{
    CanonicalVisualAsset, ConvexDecompositionError, GlbLoadLimits, VhacdResourceParameter,
    canonicalize_visual_asset, generate_convex_parts, load_glb,
};
use bop_core::{RigidTransform, RunConfig, VhacdParameters};

const CUBE_INDICES: [u32; 36] = [
    0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 3, 7, 6, 3, 6, 2, 0, 4, 7, 0, 7, 3, 1, 2,
    6, 1, 6, 5,
];

fn cube(origin: [f32; 3]) -> (Vec<[f32; 3]>, Vec<u32>) {
    let [x, y, z] = origin;
    (
        vec![
            [x, y, z],
            [x + 1.0, y, z],
            [x + 1.0, y + 1.0, z],
            [x, y + 1.0, z],
            [x, y, z + 1.0],
            [x + 1.0, y, z + 1.0],
            [x + 1.0, y + 1.0, z + 1.0],
            [x, y + 1.0, z + 1.0],
        ],
        CUBE_INDICES.to_vec(),
    )
}

fn l_prism() -> (Vec<[f32; 3]>, Vec<u32>) {
    (
        vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.0, 0.0, 1.0],
            [2.0, 0.0, 1.0],
            [2.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 2.0, 1.0],
            [0.0, 2.0, 1.0],
        ],
        vec![
            0, 2, 1, 0, 3, 2, 0, 4, 3, 0, 5, 4, 6, 7, 8, 6, 8, 9, 6, 9, 10, 6, 10, 11, 0, 1, 7, 0,
            7, 6, 1, 2, 8, 1, 8, 7, 2, 3, 9, 2, 9, 8, 3, 4, 10, 3, 10, 9, 4, 5, 11, 4, 11, 10, 5,
            0, 6, 5, 6, 11,
        ],
    )
}

fn append_f32(bytes: &mut Vec<u8>, values: impl IntoIterator<Item = f32>) {
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

fn append_u32(bytes: &mut Vec<u8>, values: &[u32]) {
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

fn glb(primitives: &[(Vec<[f32; 3]>, Vec<u32>)]) -> Vec<u8> {
    let mut binary = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut primitive_json = Vec::new();

    for (primitive_index, (positions, indices)) in primitives.iter().enumerate() {
        let position_offset = binary.len();
        append_f32(&mut binary, positions.iter().flatten().copied());
        let position_length = binary.len() - position_offset;
        let normal_offset = binary.len();
        append_f32(
            &mut binary,
            std::iter::repeat_n([0.0_f32, 0.0, 1.0], positions.len()).flatten(),
        );
        let normal_length = binary.len() - normal_offset;
        let index_offset = binary.len();
        append_u32(&mut binary, indices);
        let index_length = binary.len() - index_offset;

        let position_view = views.len();
        views.push(format!(
            r#"{{"buffer":0,"byteOffset":{position_offset},"byteLength":{position_length}}}"#
        ));
        let normal_view = views.len();
        views.push(format!(
            r#"{{"buffer":0,"byteOffset":{normal_offset},"byteLength":{normal_length}}}"#
        ));
        let index_view = views.len();
        views.push(format!(
            r#"{{"buffer":0,"byteOffset":{index_offset},"byteLength":{index_length}}}"#
        ));

        let minimum = std::array::from_fn::<_, 3, _>(|component| {
            positions
                .iter()
                .map(|position| position[component])
                .fold(f32::INFINITY, f32::min)
        });
        let maximum = std::array::from_fn::<_, 3, _>(|component| {
            positions
                .iter()
                .map(|position| position[component])
                .fold(f32::NEG_INFINITY, f32::max)
        });
        let position_accessor = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{position_view},"componentType":5126,"count":{},"type":"VEC3","min":[{},{},{}],"max":[{},{},{}]}}"#,
            positions.len(),
            minimum[0], minimum[1], minimum[2],
            maximum[0], maximum[1], maximum[2],
        ));
        let normal_accessor = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{normal_view},"componentType":5126,"count":{},"type":"VEC3"}}"#,
            positions.len()
        ));
        let index_accessor = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{index_view},"componentType":5125,"count":{},"type":"SCALAR"}}"#,
            indices.len()
        ));
        primitive_json.push(format!(
            r#"{{"attributes":{{"POSITION":{position_accessor},"NORMAL":{normal_accessor}}},"indices":{index_accessor},"mode":4,"extras":{{"fixture":{primitive_index}}}}}"#
        ));
    }

    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":{}}}],"bufferViews":[{}],"accessors":[{}],"meshes":[{{"primitives":[{}]}}],"nodes":[{{"mesh":0}}],"scenes":[{{"nodes":[0]}}],"scene":0}}"#,
        binary.len(),
        views.join(","),
        accessors.join(","),
        primitive_json.join(","),
    );
    pack_glb(&json, &binary)
}

fn pack_glb(json: &str, binary: &[u8]) -> Vec<u8> {
    const JSON_CHUNK: u32 = 0x4e4f_534a;
    const BIN_CHUNK: u32 = 0x004e_4942;
    let mut json = json.as_bytes().to_vec();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut binary = binary.to_vec();
    while !binary.len().is_multiple_of(4) {
        binary.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + binary.len();
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(b"glTF");
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(total)
            .expect("fixture length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(json.len())
            .expect("JSON length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&JSON_CHUNK.to_le_bytes());
    bytes.extend_from_slice(&json);
    bytes.extend_from_slice(
        &u32::try_from(binary.len())
            .expect("BIN length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&BIN_CHUNK.to_le_bytes());
    bytes.extend_from_slice(&binary);
    bytes
}

fn canonical(primitives: &[(Vec<[f32; 3]>, Vec<u32>)]) -> CanonicalVisualAsset {
    let limits = GlbLoadLimits::try_new(16, 8, 256, 512, 1024, 1_048_576, 2_097_152)
        .expect("fixture limits are valid");
    let loaded = load_glb(&glb(primitives), limits).expect("generated GLB loads");
    canonicalize_visual_asset(loaded, RigidTransform::identity()).expect("identity canonicalizes")
}

fn parameters(max_convex_hulls: u32) -> VhacdParameters {
    parameters_with(10_000, max_convex_hulls, 4, 4)
}

fn parameters_with(
    resolution: u32,
    max_convex_hulls: u32,
    plane_downsampling: u32,
    convex_hull_downsampling: u32,
) -> VhacdParameters {
    let mut toml = String::from(
        r#"[run]
master_seed = 1
valid_scene_count = 1
simulation_workers = 1
renderer_workers = 1
queue_capacity = 1
output_path = "output"
cache_path = "cache"
[bin]
inner_dimensions_m = [10.0, 10.0, 10.0]
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
        toml,
        "vhacd = {{ resolution = {resolution}, max_convex_hulls = {max_convex_hulls}, concavity = 0.0025, plane_downsampling = {plane_downsampling}, convex_hull_downsampling = {convex_hull_downsampling} }}"
    )
    .expect("writing String succeeds");
    toml.push_str(
        r#"[population]
object_count = { kind = "fixed", value = 1 }
asset_mixture = { kind = "fixed", value = "part" }
spawn_volume = { min_m = [-1.0, -1.0, 0.0], max_m = [1.0, 1.0, 1.0] }
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
    RunConfig::from_toml(&toml)
        .expect("parameter config parses")
        .plan()
        .assets()[0]
        .vhacd()
}

fn assert_checked_parts(parts: &[bop_assets::ConvexPart], maximum: usize) {
    assert!(!parts.is_empty());
    assert!(parts.len() <= maximum);
    for part in parts {
        assert!(part.vertices().len() >= 4);
        assert!(!part.indices().is_empty());
        assert!(part.indices().len().is_multiple_of(3));
        assert!(
            part.vertices()
                .iter()
                .flatten()
                .all(|value| value.is_finite())
        );
        assert!(
            part.indices()
                .iter()
                .all(|index| usize::try_from(*index).expect("u32 fits usize")
                    < part.vertices().len())
        );
    }
}

#[test]
fn convex_cube_produces_checked_repeatable_voxel_hulls() {
    let asset = canonical(&[cube([0.0, 0.0, 0.0])]);
    let params = parameters(8);

    let first = generate_convex_parts(&asset, params).expect("cube decomposes");
    let second = generate_convex_parts(&asset, params).expect("same cube decomposes again");

    assert_checked_parts(&first, 8);
    assert_eq!(first, second);
}

#[test]
fn disconnected_multi_primitive_mesh_uses_local_indices_and_multiple_hulls() {
    let asset = canonical(&[cube([0.0, 0.0, 0.0]), cube([3.0, 0.0, 0.0])]);

    let parts = generate_convex_parts(&asset, parameters(8)).expect("compound mesh decomposes");

    assert_checked_parts(&parts, 8);
    assert!(
        parts.len() >= 2,
        "disconnected cubes need distinct convex hulls"
    );
}

#[test]
fn configured_single_hull_limit_is_respected() {
    let asset = canonical(&[cube([0.0, 0.0, 0.0]), cube([3.0, 0.0, 0.0])]);

    let parts =
        generate_convex_parts(&asset, parameters(1)).expect("bounded decomposition succeeds");

    assert_checked_parts(&parts, 1);
}

#[test]
fn connected_concave_mesh_is_split_into_multiple_checked_hulls() {
    let asset = canonical(&[l_prism()]);

    let parts = generate_convex_parts(&asset, parameters(8)).expect("L-prism decomposes");

    assert_checked_parts(&parts, 8);
    assert!(parts.len() >= 2, "a connected L-prism is not convex");
}

#[test]
fn excessive_voxel_budget_is_rejected_before_parry_allocation() {
    let asset = canonical(&[cube([0.0, 0.0, 0.0])]);

    assert_eq!(
        generate_convex_parts(&asset, parameters_with(2_097_153, 8, 4, 4)),
        Err(ConvexDecompositionError::ResourceLimitExceeded {
            parameter: VhacdResourceParameter::VoxelBudget,
            requested: 2_097_153,
            maximum: 2_097_152,
        })
    );
}

#[test]
fn excessive_hull_count_is_rejected_before_parry_depth_arithmetic() {
    let asset = canonical(&[cube([0.0, 0.0, 0.0])]);

    assert_eq!(
        generate_convex_parts(&asset, parameters_with(10_000, 1_025, 4, 4)),
        Err(ConvexDecompositionError::ResourceLimitExceeded {
            parameter: VhacdResourceParameter::MaxConvexHulls,
            requested: 1_025,
            maximum: 1_024,
        })
    );
}

#[test]
fn excessive_plane_downsampling_is_rejected_before_parry_arithmetic() {
    let asset = canonical(&[cube([0.0, 0.0, 0.0])]);

    assert_eq!(
        generate_convex_parts(&asset, parameters_with(10_000, 8, 257, 4)),
        Err(ConvexDecompositionError::ResourceLimitExceeded {
            parameter: VhacdResourceParameter::PlaneDownsampling,
            requested: 257,
            maximum: 256,
        })
    );
}

#[test]
fn excessive_hull_downsampling_is_rejected_before_parry_arithmetic() {
    let asset = canonical(&[cube([0.0, 0.0, 0.0])]);

    assert_eq!(
        generate_convex_parts(&asset, parameters_with(10_000, 8, 4, 257)),
        Err(ConvexDecompositionError::ResourceLimitExceeded {
            parameter: VhacdResourceParameter::ConvexHullDownsampling,
            requested: 257,
            maximum: 256,
        })
    );
}

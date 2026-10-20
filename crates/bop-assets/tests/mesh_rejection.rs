//! Acceptance matrix for rejecting unusable source meshes.

use std::fmt::Write as _;

use bop_assets::{GlbLoadError, GlbLoadLimits, InvalidVisualPrimitiveError, load_glb};

fn append_f32(binary: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        binary.extend_from_slice(&value.to_le_bytes());
    }
}

fn append_u16(binary: &mut Vec<u8>, values: &[u16]) {
    for value in values {
        binary.extend_from_slice(&value.to_le_bytes());
    }
}

fn glb(json: &str, binary: &[u8]) -> Vec<u8> {
    const JSON_CHUNK: u32 = 0x4e4f_534a;
    const BIN_CHUNK: u32 = 0x004e_4942;
    let mut json_bytes = json.as_bytes().to_vec();
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }
    let mut binary_bytes = binary.to_vec();
    while !binary_bytes.len().is_multiple_of(4) {
        binary_bytes.push(0);
    }
    let total_length = 12 + 8 + json_bytes.len() + 8 + binary_bytes.len();
    let mut bytes = Vec::with_capacity(total_length);
    bytes.extend_from_slice(b"glTF");
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(total_length)
            .expect("test GLB length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(json_bytes.len())
            .expect("test JSON length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&JSON_CHUNK.to_le_bytes());
    bytes.extend_from_slice(&json_bytes);
    bytes.extend_from_slice(
        &u32::try_from(binary_bytes.len())
            .expect("test BIN length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&BIN_CHUNK.to_le_bytes());
    bytes.extend_from_slice(&binary_bytes);
    bytes
}

fn limits() -> GlbLoadLimits {
    GlbLoadLimits::try_new(8, 8, 64, 128, 1024, 1_048_576, 4_194_304)
        .expect("test limits are valid")
}

fn triangle_glb(
    positions: [[f32; 3]; 3],
    normals: Option<[[f32; 3]; 3]>,
    tex_coords: Option<[[f32; 2]; 3]>,
    indices: &[u16],
) -> Vec<u8> {
    triangle_glb_with_options(positions, normals, tex_coords, indices, 4, "")
}

fn triangle_glb_with_options(
    positions: [[f32; 3]; 3],
    normals: Option<[[f32; 3]; 3]>,
    tex_coords: Option<[[f32; 2]; 3]>,
    indices: &[u16],
    mode: u32,
    node_properties: &str,
) -> Vec<u8> {
    let position_min: [f32; 3] = std::array::from_fn(|component| {
        positions
            .iter()
            .map(|position| position[component])
            .filter(|value| value.is_finite())
            .fold(f32::INFINITY, f32::min)
    });
    let position_max: [f32; 3] = std::array::from_fn(|component| {
        positions
            .iter()
            .map(|position| position[component])
            .filter(|value| value.is_finite())
            .fold(f32::NEG_INFINITY, f32::max)
    });
    let mut binary = Vec::new();
    append_f32(&mut binary, &positions.concat());
    let mut views = vec![r#"{"buffer":0,"byteOffset":0,"byteLength":36}"#.to_owned()];
    let mut accessors = vec![format!(
        r#"{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[{},{},{}],"max":[{},{},{}]}}"#,
        position_min[0],
        position_min[1],
        position_min[2],
        position_max[0],
        position_max[1],
        position_max[2],
    )];
    let mut attributes = r#""POSITION":0"#.to_owned();

    if let Some(normals) = normals {
        let offset = binary.len();
        append_f32(&mut binary, &normals.concat());
        views.push(format!(
            r#"{{"buffer":0,"byteOffset":{offset},"byteLength":36}}"#
        ));
        accessors.push(format!(
            r#"{{"bufferView":{},"componentType":5126,"count":3,"type":"VEC3"}}"#,
            views.len() - 1
        ));
        write!(attributes, r#", "NORMAL":{}"#, accessors.len() - 1)
            .expect("writing test JSON to a String cannot fail");
    }
    if let Some(tex_coords) = tex_coords {
        let offset = binary.len();
        append_f32(&mut binary, &tex_coords.concat());
        views.push(format!(
            r#"{{"buffer":0,"byteOffset":{offset},"byteLength":24}}"#
        ));
        accessors.push(format!(
            r#"{{"bufferView":{},"componentType":5126,"count":3,"type":"VEC2"}}"#,
            views.len() - 1
        ));
        write!(attributes, r#", "TEXCOORD_0":{}"#, accessors.len() - 1)
            .expect("writing test JSON to a String cannot fail");
    }

    let index_offset = binary.len();
    append_u16(&mut binary, indices);
    views.push(format!(
        r#"{{"buffer":0,"byteOffset":{index_offset},"byteLength":{}}}"#,
        indices.len() * 2
    ));
    accessors.push(format!(
        r#"{{"bufferView":{},"componentType":5123,"count":{},"type":"SCALAR"}}"#,
        views.len() - 1,
        indices.len()
    ));
    let index_accessor = accessors.len() - 1;

    let json = format!(
        r#"{{
            "asset":{{"version":"2.0"}},
            "buffers":[{{"byteLength":{}}}],
            "bufferViews":[{}],
            "accessors":[{}],
            "meshes":[{{"primitives":[{{
                "attributes":{{{attributes}}},
                "indices":{index_accessor},
                "mode":{mode}
            }}]}}],
            "nodes":[{{"mesh":0{node_properties}}}],
            "scenes":[{{"nodes":[0]}}],
            "scene":0
        }}"#,
        binary.len(),
        views.join(","),
        accessors.join(","),
    );
    glb(&json, &binary)
}

#[test]
fn malformed_container_is_rejected() {
    assert!(matches!(
        load_glb(b"not a GLB", limits()),
        Err(GlbLoadError::Malformed { .. })
    ));
}

#[test]
fn selected_scene_without_mesh_primitives_is_rejected_as_empty() {
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":0}],
            "scenes":[{"nodes":[]}],
            "scene":0
        }"#,
        &[],
    );

    assert_eq!(load_glb(&source, limits()), Err(GlbLoadError::EmptyScene));
}

#[test]
fn non_finite_position_without_normals_is_reported_as_non_finite() {
    let source = triangle_glb(
        [[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        None,
        None,
        &[0, 1, 2],
    );

    assert_eq!(
        load_glb(&source, limits()),
        Err(GlbLoadError::InvalidPrimitive(
            InvalidVisualPrimitiveError::NonFinitePosition {
                vertex: 0,
                component: 0,
            }
        ))
    );
}

#[test]
fn finite_position_overflowing_under_finite_node_transform_is_rejected() {
    let source = triangle_glb_with_options(
        [[2.0e38, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        None,
        None,
        &[0, 1, 2],
        4,
        r#", "translation":[2.0e38,0,0]"#,
    );

    assert_eq!(
        load_glb(&source, limits()),
        Err(GlbLoadError::InvalidPrimitive(
            InvalidVisualPrimitiveError::NonFinitePosition {
                vertex: 0,
                component: 0,
            }
        ))
    );
}

#[test]
fn non_finite_declared_normal_is_rejected() {
    let source = triangle_glb(
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        Some([[0.0, f32::INFINITY, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]]),
        None,
        &[0, 1, 2],
    );

    assert!(matches!(
        load_glb(&source, limits()),
        Err(GlbLoadError::InvalidPrimitive(
            InvalidVisualPrimitiveError::NonFiniteNormal {
                vertex: 0,
                component: 0..=2,
            }
        ))
    ));
}

#[test]
fn non_finite_declared_texture_coordinate_is_rejected() {
    let source = triangle_glb(
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        Some([[0.0, 0.0, 1.0]; 3]),
        Some([[0.0, 0.0], [f32::NEG_INFINITY, 0.0], [0.0, 1.0]]),
        &[0, 1, 2],
    );

    assert_eq!(
        load_glb(&source, limits()),
        Err(GlbLoadError::InvalidPrimitive(
            InvalidVisualPrimitiveError::NonFiniteTexCoord {
                set: 0,
                vertex: 1,
                component: 0,
            }
        ))
    );
}

#[test]
fn partial_triangle_index_stream_is_rejected() {
    let source = triangle_glb(
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        Some([[0.0, 0.0, 1.0]; 3]),
        None,
        &[0, 1],
    );

    assert_eq!(
        load_glb(&source, limits()),
        Err(GlbLoadError::InvalidTriangleIndexCount {
            primitive: 0,
            count: 2,
        })
    );
}

#[test]
fn out_of_range_index_without_normals_reports_exact_index_and_offset() {
    let source = triangle_glb(
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        None,
        None,
        &[0, 7, 2],
    );

    assert_eq!(
        load_glb(&source, limits()),
        Err(GlbLoadError::InvalidPrimitive(
            InvalidVisualPrimitiveError::IndexOutOfBounds {
                index_offset: 1,
                index: 7,
                vertex_count: 3,
            }
        ))
    );
}

#[test]
fn triangle_strip_reports_post_triangulation_invalid_index_offset() {
    let source = triangle_glb_with_options(
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        None,
        None,
        &[0, 1, 2, 7],
        5,
        "",
    );

    assert_eq!(
        load_glb(&source, limits()),
        Err(GlbLoadError::InvalidPrimitive(
            InvalidVisualPrimitiveError::IndexOutOfBounds {
                index_offset: 4,
                index: 7,
                vertex_count: 3,
            }
        ))
    );
}

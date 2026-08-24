//! Public contract tests for explicit source-model to canonical transforms.

use bpp_assets::{
    CanonicalVisualAsset, CanonicalizeVisualAssetError, GlbLoadLimits, MaterialIndex,
    canonicalize_visual_asset, load_glb,
};
use bpp_core::RigidTransform;
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

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

fn textured_source_asset_with_geometry(
    positions: [[f32; 3]; 3],
    normals: [[f32; 3]; 3],
) -> bpp_assets::LoadedVisualAsset {
    let position_min: [f32; 3] = std::array::from_fn(|component| {
        positions
            .iter()
            .map(|position| position[component])
            .fold(f32::INFINITY, f32::min)
    });
    let position_max: [f32; 3] = std::array::from_fn(|component| {
        positions
            .iter()
            .map(|position| position[component])
            .fold(f32::NEG_INFINITY, f32::max)
    });
    let mut binary = Vec::new();
    append_f32(&mut binary, &positions.concat());
    append_f32(&mut binary, &normals.concat());
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    append_u16(&mut binary, &[0, 1, 2]);
    while !binary.len().is_multiple_of(4) {
        binary.push(0);
    }
    let image_offset = binary.len();
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&[10, 20, 30, 255], 1, 1, ExtendedColorType::Rgba8)
        .expect("test PNG encodes");
    binary.extend_from_slice(&png);
    let json = format!(
        r#"{{
            "asset":{{"version":"2.0"}},
            "buffers":[{{"byteLength":{buffer_length}}}],
            "bufferViews":[
                {{"buffer":0,"byteOffset":0,"byteLength":36}},
                {{"buffer":0,"byteOffset":36,"byteLength":36}},
                {{"buffer":0,"byteOffset":72,"byteLength":24}},
                {{"buffer":0,"byteOffset":96,"byteLength":6}},
                {{"buffer":0,"byteOffset":{image_offset},"byteLength":{image_length}}}
            ],
            "accessors":[
                {{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3",
                  "min":[{min_x},{min_y},{min_z}],"max":[{max_x},{max_y},{max_z}]}},
                {{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}},
                {{"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"}},
                {{"bufferView":3,"componentType":5123,"count":3,"type":"SCALAR"}}
            ],
            "images":[{{"bufferView":4,"mimeType":"image/png"}}],
            "textures":[{{"source":0}}],
            "materials":[{{"pbrMetallicRoughness":{{
                "baseColorFactor":[0.25,0.5,0.75,1.0],
                "baseColorTexture":{{"index":0,"texCoord":0}}
            }}}}],
            "meshes":[{{"primitives":[{{
                "attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},
                "indices":3,
                "material":0
            }},{{
                "attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},
                "indices":3
            }}]}}],
            "nodes":[{{"mesh":0}}],
            "scenes":[{{"nodes":[0]}}],
            "scene":0
        }}"#,
        buffer_length = binary.len(),
        image_length = png.len(),
        min_x = position_min[0],
        min_y = position_min[1],
        min_z = position_min[2],
        max_x = position_max[0],
        max_y = position_max[1],
        max_z = position_max[2],
    );
    let limits = GlbLoadLimits::try_new(8, 8, 32, 64, 1024, 1_048_576, 4_194_304)
        .expect("test limits are nonzero");
    load_glb(&glb(&json, &binary), limits).expect("test GLB loads")
}

fn textured_source_asset() -> bpp_assets::LoadedVisualAsset {
    textured_source_asset_with_geometry(
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        [[1.0, 1.0, 1.0]; 3],
    )
}

fn assert_vec3_near(actual: [f32; 3], expected: [f32; 3]) {
    for (component, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
        assert!(
            (actual - expected).abs() <= 4.0 * f32::EPSILON,
            "component {component}: expected {expected}, got {actual}"
        );
    }
}

#[test]
fn public_asset_stage_applies_explicit_y_up_to_z_up_rotation() {
    let canonical_from_model =
        RigidTransform::try_new([0.0, 0.0, 0.0], [0.5_f32.sqrt(), 0.0, 0.0, 0.5_f32.sqrt()])
            .expect("literal basis rotation is valid");

    let canonical = canonicalize_visual_asset(textured_source_asset(), canonical_from_model)
        .expect("finite rigid transform succeeds");
    let primitive = &canonical.visual_mesh().primitives()[0];

    for (actual, expected) in primitive.positions().iter().copied().zip([
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
    ]) {
        assert_vec3_near(actual, expected);
    }
    let unit = 1.0_f32 / 3.0_f32.sqrt();
    for normal in primitive.normals() {
        assert_vec3_near(*normal, [unit, -unit, unit]);
    }
}

#[test]
fn identity_preserves_complete_mesh_material_and_texture_tables_without_cloning_pixels() {
    let source = textured_source_asset();
    let source_mesh = source.visual_mesh().clone();
    let source_materials = source.materials().to_vec();
    let source_textures = source.textures().to_vec();
    let source_pixels = source.textures()[0].image().rgba8_srgb().as_ptr();

    let canonical: CanonicalVisualAsset =
        canonicalize_visual_asset(source, RigidTransform::identity())
            .expect("identity canonicalization succeeds");

    assert_eq!(canonical.visual_mesh(), &source_mesh);
    assert_eq!(canonical.materials(), source_materials);
    assert_eq!(canonical.textures(), source_textures);
    assert!(std::ptr::eq(
        source_pixels,
        canonical.textures()[0].image().rgba8_srgb().as_ptr(),
    ));
}

#[test]
fn translation_does_not_recenter_and_non_geometry_streams_keep_order() {
    let source = textured_source_asset();
    let source_primitives = source.visual_mesh().primitives().to_vec();
    let canonical_from_model = RigidTransform::try_new([10.0, -2.0, 5.0], [0.0, 0.0, 0.0, 1.0])
        .expect("literal translation is valid");

    let canonical = canonicalize_visual_asset(source, canonical_from_model)
        .expect("finite translation succeeds");

    assert_eq!(
        canonical.visual_mesh().primitives()[0].positions(),
        &[[10.0, -2.0, 5.0], [11.0, -2.0, 5.0], [10.0, -1.0, 5.0]]
    );
    for (actual, expected) in canonical
        .visual_mesh()
        .primitives()
        .iter()
        .zip(source_primitives)
    {
        assert_eq!(actual.indices(), expected.indices());
        assert_eq!(actual.tex_coord_sets(), expected.tex_coord_sets());
        assert_eq!(actual.material(), expected.material());
    }
    assert_eq!(
        canonical.visual_mesh().primitives()[0].material(),
        Some(MaterialIndex::new(0))
    );
    assert_eq!(
        canonical.visual_mesh().primitives()[1].material(),
        Some(MaterialIndex::new(1))
    );
}

#[test]
fn transformed_position_overflow_is_a_typed_error() {
    let source = textured_source_asset_with_geometry(
        [
            [f32::MAX, 0.0, 0.0],
            [f32::MAX, 1.0, 0.0],
            [f32::MAX, 0.0, 1.0],
        ],
        [[1.0, 0.0, 0.0]; 3],
    );
    let canonical_from_model = RigidTransform::try_new([f32::MAX, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0])
        .expect("large finite translation is valid");

    assert_eq!(
        canonicalize_visual_asset(source, canonical_from_model).unwrap_err(),
        CanonicalizeVisualAssetError::PositionOutOfRange {
            primitive: 0,
            vertex: 0,
        }
    );
}

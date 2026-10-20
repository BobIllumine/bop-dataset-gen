//! Public integration coverage for bounded self-contained GLB ingestion.

use bop_assets::{
    GlbLoadError, GlbLoadLimits, GlbMaterialFeature, MagnificationFilter, MinificationFilter,
    UnsupportedGlbFeature, WrapMode, load_glb,
};
use image::{
    ExtendedColorType, ImageEncoder,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
};

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

fn assert_f32_near(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= f32::EPSILON,
        "expected {expected}, got {actual}"
    );
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

fn glb_without_bin(json: &str) -> Vec<u8> {
    const JSON_CHUNK: u32 = 0x4e4f_534a;

    let mut json_bytes = json.as_bytes().to_vec();
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }
    let total_length = 12 + 8 + json_bytes.len();
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
    bytes
}

fn red_png() -> Vec<u8> {
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded)
        .write_image(&[255, 0, 0, 255], 1, 1, ExtendedColorType::Rgba8)
        .expect("test PNG encodes");
    encoded
}

fn red_jpeg() -> Vec<u8> {
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, 100)
        .write_image(&[255, 0, 0], 1, 1, ExtendedColorType::Rgb8)
        .expect("test JPEG encodes");
    encoded
}

fn solid_red_png(width: u32, height: u32) -> Vec<u8> {
    let pixel_count = usize::try_from(width)
        .expect("test width fits usize")
        .checked_mul(usize::try_from(height).expect("test height fits usize"))
        .expect("test pixel count fits usize");
    let mut pixels = Vec::with_capacity(pixel_count * 4);
    for _ in 0..pixel_count {
        pixels.extend_from_slice(&[255, 0, 0, 255]);
    }
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded)
        .write_image(&pixels, width, height, ExtendedColorType::Rgba8)
        .expect("test PNG encodes");
    encoded
}

fn image_only_glb(width: u32, height: u32) -> (Vec<u8>, usize) {
    let png = solid_red_png(width, height);
    let json = format!(
        r#"{{
            "asset":{{"version":"2.0"}},
            "buffers":[{{"byteLength":{length}}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{length}}}],
            "images":[{{"bufferView":0,"mimeType":"image/png"}}],
            "scenes":[{{"nodes":[]}}],
            "scene":0
        }}"#,
        length = png.len(),
    );
    (glb(&json, &png), png.len())
}

fn generous_limits() -> GlbLoadLimits {
    GlbLoadLimits::try_new(16, 16, 64, 128, 1024, 1_048_576, 4_194_304)
        .expect("test limits are valid")
}

fn nonindexed_quad_glb(mode: u32, node_properties: &str) -> Vec<u8> {
    let mut binary = Vec::new();
    append_f32(
        &mut binary,
        &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0],
    );
    append_f32(
        &mut binary,
        &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
    );
    let json = format!(
        r#"{{
            "asset":{{"version":"2.0"}},
            "buffers":[{{"byteLength":96}}],
            "bufferViews":[
                {{"buffer":0,"byteOffset":0,"byteLength":48}},
                {{"buffer":0,"byteOffset":48,"byteLength":48}}
            ],
            "accessors":[
                {{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}},
                {{"bufferView":1,"componentType":5126,"count":4,"type":"VEC3"}}
            ],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1}},"mode":{mode}}}]}}],
            "nodes":[{{"mesh":0{node_properties}}}],
            "scenes":[{{"nodes":[0]}}],
            "scene":0
        }}"#
    );
    glb(&json, &binary)
}

fn material_only_glb(material_json: &str) -> Vec<u8> {
    let json = format!(
        r#"{{
            "asset":{{"version":"2.0"}},
            "buffers":[{{"byteLength":0}}],
            "materials":[{material_json}],
            "scenes":[{{"nodes":[]}}],
            "scene":0
        }}"#
    );
    glb(&json, &[])
}

fn textured_triangle_glb(tex_coord_set: u32) -> Vec<u8> {
    textured_triangle_glb_with_image(tex_coord_set, &red_png(), "image/png")
}

fn textured_triangle_glb_with_image(
    tex_coord_set: u32,
    encoded_image: &[u8],
    mime_type: &str,
) -> Vec<u8> {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    append_u16(&mut binary, &[0, 1, 2]);
    while !binary.len().is_multiple_of(4) {
        binary.push(0);
    }
    let image_offset = binary.len();
    binary.extend_from_slice(encoded_image);
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
                {{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}},
                {{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}},
                {{"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"}},
                {{"bufferView":3,"componentType":5123,"count":3,"type":"SCALAR"}}
            ],
            "images":[{{"bufferView":4,"mimeType":"{mime_type}"}}],
            "samplers":[{{"magFilter":9728,"minFilter":9987,"wrapS":33071,"wrapT":33648}}],
            "textures":[{{"sampler":0,"source":0}}],
            "materials":[{{"pbrMetallicRoughness":{{"baseColorTexture":{{"index":0,"texCoord":{tex_coord_set}}}}}}}],
            "meshes":[{{"primitives":[{{
                "attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},
                "indices":3,
                "material":0
            }}]}}],
            "nodes":[{{"mesh":0}}],
            "scenes":[{{"nodes":[0]}}],
            "scene":0
        }}"#,
        buffer_length = binary.len(),
        image_length = encoded_image.len(),
    );
    glb(&json, &binary)
}

fn triangle_glb_with_primitive_additions(
    attribute_additions: &str,
    primitive_additions: &str,
) -> Vec<u8> {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    append_u16(&mut binary, &[0, 1, 2]);
    let json = format!(
        r#"{{
            "asset":{{"version":"2.0"}},
            "buffers":[{{"byteLength":78}}],
            "bufferViews":[
                {{"buffer":0,"byteOffset":0,"byteLength":36}},
                {{"buffer":0,"byteOffset":36,"byteLength":36}},
                {{"buffer":0,"byteOffset":72,"byteLength":6}}
            ],
            "accessors":[
                {{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}},
                {{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}},
                {{"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"}}
            ],
            "meshes":[{{"primitives":[{{
                "attributes":{{"POSITION":0,"NORMAL":1{attribute_additions}}},
                "indices":2{primitive_additions}
            }}]}}],
            "nodes":[{{"mesh":0}}],
            "scenes":[{{"nodes":[0]}}],
            "scene":0
        }}"#
    );
    glb(&json, &binary)
}

fn triangle_glb_with_extra_attribute(
    semantic: &str,
    component_type: u32,
    dimensions: &str,
    extra: &[u8],
) -> Vec<u8> {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    binary.extend_from_slice(extra);
    let json = format!(
        r#"{{
            "asset":{{"version":"2.0"}},
            "buffers":[{{"byteLength":{buffer_length}}}],
            "bufferViews":[
                {{"buffer":0,"byteOffset":0,"byteLength":36}},
                {{"buffer":0,"byteOffset":36,"byteLength":36}},
                {{"buffer":0,"byteOffset":72,"byteLength":{extra_length}}}
            ],
            "accessors":[
                {{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}},
                {{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}},
                {{"bufferView":2,"componentType":{component_type},"count":3,"type":"{dimensions}"}}
            ],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1,"{semantic}":2}}}}]}}],
            "nodes":[{{"mesh":0}}],
            "scenes":[{{"nodes":[0]}}],
            "scene":0
        }}"#,
        buffer_length = binary.len(),
        extra_length = extra.len(),
    );
    glb(&json, &binary)
}

fn two_instance_quad_glb() -> Vec<u8> {
    let source = nonindexed_quad_glb(5, "");
    let json_start = 20;
    let json_length = u32::from_le_bytes(source[12..16].try_into().expect("chunk length")) as usize;
    let json_end = json_start + json_length;
    let json = std::str::from_utf8(&source[json_start..json_end])
        .expect("fixture JSON")
        .trim_end();
    let json = json
        .replace(
            r#""nodes":[{"mesh":0}]"#,
            r#""nodes":[{"mesh":0},{"mesh":0}]"#,
        )
        .replace(
            r#""scenes":[{"nodes":[0]}]"#,
            r#""scenes":[{"nodes":[0,1]}]"#,
        );
    let binary_start = json_end + 8;
    glb(&json, &source[binary_start..])
}

fn duplicate_texture_glb() -> Vec<u8> {
    let source = textured_triangle_glb(0);
    let json_start = 20;
    let json_length = usize::try_from(u32::from_le_bytes(
        source[12..16].try_into().expect("chunk length"),
    ))
    .expect("test JSON length fits usize");
    let json_end = json_start + json_length;
    let json = std::str::from_utf8(&source[json_start..json_end])
        .expect("fixture JSON")
        .trim_end()
        .replace(
            r#""textures":[{"sampler":0,"source":0}]"#,
            r#""textures":[{"sampler":0,"source":0},{"sampler":0,"source":0}]"#,
        );
    let binary_start = json_end + 8;
    glb(&json, &source[binary_start..])
}

#[test]
fn indexed_triangle_is_loaded_in_metres_with_intrinsic_node_transform() {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    append_u16(&mut binary, &[0, 1, 2]);
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":78}],
            "bufferViews":[
                {"buffer":0,"byteOffset":0,"byteLength":36},
                {"buffer":0,"byteOffset":36,"byteLength":36},
                {"buffer":0,"byteOffset":72,"byteLength":6}
            ],
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
                {"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"}
            ],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1},"indices":2}]}],
            "nodes":[{"mesh":0,"translation":[1,2,3]}],
            "scenes":[{"nodes":[0]}],
            "scene":0
        }"#,
        &binary,
    );

    let asset = load_glb(&source, generous_limits()).expect("valid GLB loads");
    let [primitive] = asset.visual_mesh().primitives() else {
        panic!("one source primitive produces one loaded primitive");
    };

    assert_eq!(
        primitive.positions(),
        &[[1.0, 2.0, 3.0], [2.0, 2.0, 3.0], [1.0, 3.0, 3.0]]
    );
    assert_eq!(primitive.normals(), &[[0.0, 0.0, 1.0]; 3]);
    assert_eq!(primitive.indices(), &[0, 1, 2]);
}

#[test]
fn nonindexed_triangle_without_normals_gets_flat_generated_normals() {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0, 0.0]);
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":36}],
            "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36}],
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[2,2,0]}
            ],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "nodes":[{"mesh":0}],
            "scenes":[{"nodes":[0]}],
            "scene":0
        }"#,
        &binary,
    );

    let asset = load_glb(&source, generous_limits()).expect("valid GLB loads");
    let primitive = &asset.visual_mesh().primitives()[0];

    assert_eq!(primitive.normals(), &[[0.0, 0.0, 1.0]; 3]);
    assert_eq!(primitive.indices(), &[0, 1, 2]);
}

#[test]
fn triangle_strip_is_converted_to_stable_triangle_list() {
    let source = nonindexed_quad_glb(5, "");

    let asset = load_glb(&source, generous_limits()).expect("triangle strip loads");

    assert_eq!(
        asset.visual_mesh().primitives()[0].indices(),
        &[0, 1, 2, 1, 3, 2]
    );
}

#[test]
fn triangle_fan_is_converted_to_stable_triangle_list() {
    let source = nonindexed_quad_glb(6, "");

    let asset = load_glb(&source, generous_limits()).expect("triangle fan loads");

    assert_eq!(
        asset.visual_mesh().primitives()[0].indices(),
        &[0, 1, 2, 0, 2, 3]
    );
}

#[test]
fn reflected_node_transform_flips_winding_to_match_transformed_normals() {
    let source = nonindexed_quad_glb(5, r#", "scale":[-1,1,1]"#);

    let asset = load_glb(&source, generous_limits()).expect("reflected triangle loads");
    let primitive = &asset.visual_mesh().primitives()[0];

    assert_eq!(primitive.indices(), &[0, 2, 1, 1, 2, 3]);
    assert_eq!(primitive.normals(), &[[0.0, 0.0, 1.0]; 4]);
}

#[test]
fn pbr_factors_material_assignment_and_uvs_are_preserved() {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    append_u16(&mut binary, &[0, 1, 2]);
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":102}],
            "bufferViews":[
                {"buffer":0,"byteOffset":0,"byteLength":36},
                {"buffer":0,"byteOffset":36,"byteLength":36},
                {"buffer":0,"byteOffset":72,"byteLength":24},
                {"buffer":0,"byteOffset":96,"byteLength":6}
            ],
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
                {"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"},
                {"bufferView":3,"componentType":5123,"count":3,"type":"SCALAR"}
            ],
            "materials":[{"pbrMetallicRoughness":{
                "baseColorFactor":[0.2,0.4,0.6,0.8],
                "metallicFactor":0.3,
                "roughnessFactor":0.7
            }}],
            "meshes":[{"primitives":[{
                "attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},
                "indices":3,
                "material":0
            }]}],
            "nodes":[{"mesh":0}],
            "scenes":[{"nodes":[0]}],
            "scene":0
        }"#,
        &binary,
    );

    let asset = load_glb(&source, generous_limits()).expect("supported material loads");
    let primitive = &asset.visual_mesh().primitives()[0];
    let material = &asset.materials()[0];

    assert_eq!(
        primitive.material().map(bop_assets::MaterialIndex::get),
        Some(0)
    );
    assert_eq!(
        primitive.tex_coord_sets(),
        &[vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]]
    );
    for (actual, expected) in material
        .base_color_factor()
        .into_iter()
        .zip([0.2, 0.4, 0.6, 0.8])
    {
        assert_f32_near(actual, expected);
    }
    assert_f32_near(material.metallic_factor(), 0.3);
    assert_f32_near(material.roughness_factor(), 0.7);
    assert_eq!(material.base_color_texture(), None);
}

#[test]
fn embedded_base_color_texture_pixels_binding_and_sampler_are_preserved() {
    let source = textured_triangle_glb(0);

    let asset = load_glb(&source, generous_limits()).expect("textured GLB loads");
    let binding = asset.materials()[0]
        .base_color_texture()
        .expect("base-color binding is preserved");
    let texture = &asset.textures()[0];

    assert_eq!(binding.texture().get(), 0);
    assert_eq!(binding.tex_coord_set(), 0);
    assert_eq!(texture.image().dimensions(), [1, 1]);
    assert_eq!(texture.image().rgba8_srgb(), &[255, 0, 0, 255]);
    assert_eq!(
        texture.sampler().magnification(),
        Some(MagnificationFilter::Nearest)
    );
    assert_eq!(
        texture.sampler().minification(),
        Some(MinificationFilter::LinearMipmapLinear)
    );
    assert_eq!(texture.sampler().wrap_u(), WrapMode::ClampToEdge);
    assert_eq!(texture.sampler().wrap_v(), WrapMode::MirroredRepeat);
}

#[test]
fn embedded_jpeg_base_color_texture_is_decoded_to_rgba8() {
    let source = textured_triangle_glb_with_image(0, &red_jpeg(), "image/jpeg");

    let asset = load_glb(&source, generous_limits()).expect("textured JPEG GLB loads");
    let pixel = asset.textures()[0].image().rgba8_srgb();

    assert_eq!(asset.textures()[0].image().dimensions(), [1, 1]);
    assert!(pixel[0] > 240, "red channel was {}", pixel[0]);
    assert!(pixel[1] < 16, "green channel was {}", pixel[1]);
    assert!(pixel[2] < 16, "blue channel was {}", pixel[2]);
    assert_eq!(pixel[3], 255);
}

#[test]
fn repeated_texture_references_share_decoded_image_storage() {
    let source = duplicate_texture_glb();

    let asset = load_glb(&source, generous_limits()).expect("duplicate texture references load");

    assert_eq!(asset.textures().len(), 2);
    assert!(std::ptr::eq(
        asset.textures()[0].image().rgba8_srgb().as_ptr(),
        asset.textures()[1].image().rgba8_srgb().as_ptr(),
    ));
}

#[test]
fn texture_table_limit_is_enforced_before_cloning_image_handles() {
    let source = duplicate_texture_glb();
    let limits = generous_limits()
        .try_with_structural_limits(source.len(), source.len(), source.len(), 16, 1, 16, 4)
        .expect("structural limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::Textures,
            limit: 1,
        })
    ));
}

#[test]
fn material_texture_binding_to_missing_uv_set_is_rejected() {
    let source = textured_triangle_glb(1);

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::MissingTexCoordSet {
            primitive: 0,
            material: 0,
            set: 1,
            available: 1,
        })
    ));
}

#[test]
fn triangle_list_with_partial_final_triangle_is_rejected() {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    append_u16(&mut binary, &[0, 1, 2, 0]);
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":44}],
            "bufferViews":[
                {"buffer":0,"byteOffset":0,"byteLength":36},
                {"buffer":0,"byteOffset":36,"byteLength":8}
            ],
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                {"bufferView":1,"componentType":5123,"count":4,"type":"SCALAR"}
            ],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
            "nodes":[{"mesh":0}],
            "scenes":[{"nodes":[0]}],
            "scene":0
        }"#,
        &binary,
    );

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::InvalidTriangleIndexCount {
            primitive: 0,
            count: 4,
        })
    ));
}

#[test]
fn animation_is_rejected_instead_of_loading_only_the_bind_pose() {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 1.0]);
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":32}],
            "bufferViews":[
                {"buffer":0,"byteOffset":0,"byteLength":8},
                {"buffer":0,"byteOffset":8,"byteLength":24}
            ],
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]},
                {"bufferView":1,"componentType":5126,"count":2,"type":"VEC3"}
            ],
            "nodes":[{}],
            "scenes":[{"nodes":[0]}],
            "scene":0,
            "animations":[{
                "samplers":[{"input":0,"output":1}],
                "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]
            }]
        }"#,
        &binary,
    );

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedFeature {
            feature: UnsupportedGlbFeature::Animation,
        })
    ));
}

#[test]
fn declared_glb_length_shorter_than_header_is_a_typed_error_not_a_panic() {
    let mut source = Vec::new();
    source.extend_from_slice(b"glTF");
    source.extend_from_slice(&2_u32.to_le_bytes());
    source.extend_from_slice(&8_u32.to_le_bytes());

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::Malformed { .. })
    ));
}

#[test]
fn container_byte_limit_is_enforced_before_parsing() {
    let source = nonindexed_quad_glb(5, "");
    let limits = generous_limits()
        .try_with_structural_limits(source.len() - 1, source.len(), source.len(), 16, 16, 16, 4)
        .expect("structural limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::ContainerBytes,
            limit,
        }) if limit == source.len() - 1
    ));
}

#[test]
fn json_and_binary_chunk_limits_are_enforced_before_parsing() {
    let source = nonindexed_quad_glb(5, "");
    let json_limited = generous_limits()
        .try_with_structural_limits(source.len(), 1, source.len(), 16, 16, 16, 4)
        .expect("structural limits are nonzero");
    let binary_limited = generous_limits()
        .try_with_structural_limits(source.len(), source.len(), 1, 16, 16, 16, 4)
        .expect("structural limits are nonzero");

    assert!(matches!(
        load_glb(&source, json_limited),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::JsonBytes,
            limit: 1,
        })
    ));
    assert!(matches!(
        load_glb(&source, binary_limited),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::BinaryBytes,
            limit: 1,
        })
    ));
}

#[test]
fn structural_limits_reject_zero_values() {
    assert!(matches!(
        generous_limits().try_with_structural_limits(0, 1, 1, 1, 1, 1, 1),
        Err(error) if error.field == bop_assets::GlbLimitField::ContainerBytes
    ));
}

#[test]
fn material_table_limit_is_enforced_before_material_allocation() {
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":0}],
            "materials":[{},{}],
            "scenes":[{"nodes":[]}],
            "scene":0
        }"#,
        &[],
    );
    let limits = generous_limits()
        .try_with_structural_limits(source.len(), source.len(), source.len(), 1, 16, 16, 4)
        .expect("structural limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::Materials,
            limit: 1,
        })
    ));
}

#[test]
fn declared_but_truncated_normals_are_rejected_instead_of_generated() {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":72}],
            "bufferViews":[
                {"buffer":0,"byteOffset":0,"byteLength":36},
                {"buffer":0,"byteOffset":36,"byteLength":36}
            ],
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}
            ],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1}}]}],
            "nodes":[{"mesh":0}],
            "scenes":[{"nodes":[0]}],
            "scene":0
        }"#,
        &binary,
    );

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnreadableAccessor {
            primitive: 0,
            semantic,
        }) if semantic == "NORMAL"
    ));
}

#[test]
fn declared_but_truncated_indices_are_rejected_instead_of_replaced() {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":78}],
            "bufferViews":[
                {"buffer":0,"byteOffset":0,"byteLength":36},
                {"buffer":0,"byteOffset":36,"byteLength":36},
                {"buffer":0,"byteOffset":72,"byteLength":6}
            ],
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
                {"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"}
            ],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1},"indices":2}]}],
            "nodes":[{"mesh":0}],
            "scenes":[{"nodes":[0]}],
            "scene":0
        }"#,
        &binary,
    );

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnreadableAccessor {
            primitive: 0,
            semantic,
        }) if semantic == "INDICES"
    ));
}

#[test]
fn declared_but_truncated_uvs_are_rejected_instead_of_dropped() {
    let mut binary = Vec::new();
    append_f32(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    append_f32(&mut binary, &[0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0]);
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":96}],
            "bufferViews":[
                {"buffer":0,"byteOffset":0,"byteLength":36},
                {"buffer":0,"byteOffset":36,"byteLength":36},
                {"buffer":0,"byteOffset":72,"byteLength":24}
            ],
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
                {"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"}
            ],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}}]}],
            "nodes":[{"mesh":0}],
            "scenes":[{"nodes":[0]}],
            "scene":0
        }"#,
        &binary,
    );

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnreadableAccessor {
            primitive: 0,
            semantic,
        }) if semantic == "TEXCOORD_0"
    ));
}

#[test]
fn tangent_attribute_is_rejected_instead_of_dropped() {
    let mut tangents = Vec::new();
    append_f32(
        &mut tangents,
        &[1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0],
    );
    let source = triangle_glb_with_extra_attribute("TANGENT", 5126, "VEC4", &tangents);

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedFeature {
            feature: UnsupportedGlbFeature::Tangents,
        })
    ));
}

#[test]
fn joint_attribute_is_rejected_instead_of_dropped() {
    let joints = [0_u8; 12];
    let source = triangle_glb_with_extra_attribute("JOINTS_0", 5121, "VEC4", &joints);

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedFeature {
            feature: UnsupportedGlbFeature::JointAttributes,
        })
    ));
}

#[test]
fn weight_attribute_is_rejected_instead_of_dropped() {
    let mut weights = Vec::new();
    append_f32(
        &mut weights,
        &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
    );
    let source = triangle_glb_with_extra_attribute("WEIGHTS_0", 5126, "VEC4", &weights);

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedFeature {
            feature: UnsupportedGlbFeature::WeightAttributes,
        })
    ));
}

#[test]
fn uv_set_gap_is_rejected_instead_of_dropping_higher_sets() {
    let mut uvs = Vec::new();
    append_f32(&mut uvs, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    let source = triangle_glb_with_extra_attribute("TEXCOORD_1", 5126, "VEC2", &uvs);

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::NonContiguousTexCoordSets {
            primitive: 0,
            expected: 0,
            found: 1,
        })
    ));
}

#[test]
fn transparent_material_is_rejected_instead_of_rendering_opaque() {
    let source = material_only_glb(r#"{"alphaMode":"BLEND"}"#);

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedMaterialFeature {
            material: 0,
            feature: GlbMaterialFeature::AlphaMode,
        })
    ));
}

#[test]
fn double_sided_material_is_rejected_instead_of_enabling_backface_culling() {
    let source = material_only_glb(r#"{"doubleSided":true}"#);

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedMaterialFeature {
            material: 0,
            feature: GlbMaterialFeature::DoubleSided,
        })
    ));
}

#[test]
fn emissive_material_is_rejected_instead_of_losing_emitted_light() {
    let source = material_only_glb(r#"{"emissiveFactor":[0.1,0.2,0.3]}"#);

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedMaterialFeature {
            material: 0,
            feature: GlbMaterialFeature::Emissive,
        })
    ));
}

#[test]
fn external_buffer_is_rejected_before_missing_binary_chunk() {
    let source = glb_without_bin(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"uri":"mesh.bin","byteLength":12}]
        }"#,
    );

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::ExternalBuffer { buffer: 0 })
    ));
}

#[test]
fn multiple_bin_buffers_are_rejected_as_an_ambiguous_glb_layout() {
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":0},{"byteLength":0}],
            "scenes":[{"nodes":[]}],
            "scene":0
        }"#,
        &[],
    );

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedBufferLayout { count: 2 })
    ));
}

#[test]
fn declared_extension_is_rejected_instead_of_being_silently_ignored() {
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "extensionsUsed":["KHR_materials_unlit"],
            "buffers":[{"byteLength":0}],
            "scenes":[{"nodes":[]}],
            "scene":0
        }"#,
        &[],
    );

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedExtension { name })
            if name == "KHR_materials_unlit"
    ));
}

#[test]
fn vertex_colors_are_rejected_instead_of_being_dropped() {
    let source = triangle_glb_with_primitive_additions(r#", "COLOR_0":1"#, "");

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedFeature {
            feature: UnsupportedGlbFeature::VertexColors,
        })
    ));
}

#[test]
fn morph_targets_are_rejected_instead_of_loading_only_base_shape() {
    let source = triangle_glb_with_primitive_additions("", r#", "targets":[{"POSITION":0}]"#);

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::UnsupportedFeature {
            feature: UnsupportedGlbFeature::MorphTargets,
        })
    ));
}

#[test]
fn cumulative_node_limit_is_enforced() {
    let source = two_instance_quad_glb();
    let limits = GlbLoadLimits::try_new(1, 16, 64, 128, 1024, 1_048_576, 4_194_304)
        .expect("limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::Nodes,
            limit: 1,
        })
    ));
}

#[test]
fn cumulative_primitive_limit_counts_scene_instances() {
    let source = two_instance_quad_glb();
    let limits = GlbLoadLimits::try_new(2, 1, 64, 128, 1024, 1_048_576, 4_194_304)
        .expect("limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::Primitives,
            limit: 1,
        })
    ));
}

#[test]
fn vertex_limit_counts_emitted_vertices() {
    let source = nonindexed_quad_glb(5, "");
    let limits = GlbLoadLimits::try_new(16, 16, 3, 128, 1024, 1_048_576, 4_194_304)
        .expect("limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::Vertices,
            limit: 3,
        })
    ));
}

#[test]
fn index_limit_counts_triangulated_output() {
    let source = nonindexed_quad_glb(5, "");
    let limits = GlbLoadLimits::try_new(16, 16, 64, 5, 1024, 1_048_576, 4_194_304)
        .expect("limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::Indices,
            limit: 5,
        })
    ));
}

#[test]
fn encoded_image_byte_limit_is_enforced_before_decode() {
    let (source, encoded_bytes) = image_only_glb(1, 1);
    let limits = GlbLoadLimits::try_new(16, 16, 64, 128, 1024, encoded_bytes - 1, 4_194_304)
        .expect("limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::EncodedImageBytes,
            limit,
        }) if limit == encoded_bytes - 1
    ));
}

#[test]
fn texture_dimension_limit_is_enforced_from_encoded_header() {
    let (source, _) = image_only_glb(2, 1);
    let limits = GlbLoadLimits::try_new(16, 16, 64, 128, 1, 1_048_576, 4_194_304)
        .expect("limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::TextureDimension,
            limit: 1,
        })
    ));
}

#[test]
fn decoded_texture_byte_limit_is_enforced_before_decode() {
    let (source, _) = image_only_glb(1, 1);
    let limits =
        GlbLoadLimits::try_new(16, 16, 64, 128, 1024, 1_048_576, 3).expect("limits are nonzero");

    assert!(matches!(
        load_glb(&source, limits),
        Err(GlbLoadError::LimitExceeded {
            field: bop_assets::GlbLimitField::DecodedTextureBytes,
            limit: 3,
        })
    ));
}

#[test]
fn external_image_uri_is_rejected_without_filesystem_access() {
    let source = glb(
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":0}],
            "images":[{"uri":"texture.png"}],
            "scenes":[{"nodes":[]}],
            "scene":0
        }"#,
        &[],
    );

    assert!(matches!(
        load_glb(&source, generous_limits()),
        Err(GlbLoadError::ExternalImage { image: 0 })
    ));
}

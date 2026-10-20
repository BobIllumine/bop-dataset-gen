//! Public contract tests for normalized renderer-neutral assets.

use bop_assets::{
    ConvexPart, InvalidConvexPartError, InvalidNormalizedAssetError, InvalidPbrMaterialError,
    InvalidTextureImageError, InvalidVisualMeshError, InvalidVisualPrimitiveError,
    MagnificationFilter, MaterialIndex, MinificationFilter, NormalizedAsset, PbrMaterial, Texture,
    TextureBinding, TextureImage, TextureIndex, TextureSampler, VisualMesh, VisualPrimitive,
    WrapMode,
};

fn triangle(material: Option<MaterialIndex>, with_uv: bool) -> VisualPrimitive {
    VisualPrimitive::try_new(
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0.0, 0.0, 2.0]; 3],
        if with_uv {
            vec![vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]]
        } else {
            vec![]
        },
        vec![0, 1, 2],
        material,
    )
    .expect("literal triangle is valid")
}

fn tetrahedron() -> ConvexPart {
    ConvexPart::try_new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
    )
    .expect("literal tetrahedron is valid")
}

fn texture() -> Texture {
    Texture::new(
        TextureImage::try_new(1, 1, vec![64, 128, 255, 192]).expect("one RGBA texel is valid"),
        TextureSampler::new(
            Some(MagnificationFilter::Linear),
            Some(MinificationFilter::LinearMipmapLinear),
            WrapMode::Repeat,
            WrapMode::ClampToEdge,
        ),
    )
}

fn textured_material(binding: TextureBinding) -> PbrMaterial {
    PbrMaterial::try_new([0.25, 0.5, 0.75, 1.0], 0.2, 0.8, Some(binding))
        .expect("literal PBR factors are valid")
}

#[test]
fn normalized_asset_exposes_checked_renderer_neutral_data() {
    let asset = NormalizedAsset::try_new(
        VisualMesh::try_new(vec![triangle(Some(MaterialIndex::new(0)), true)])
            .expect("one primitive is a valid mesh"),
        vec![textured_material(TextureBinding::new(
            TextureIndex::new(0),
            0,
        ))],
        vec![texture()],
        vec![tetrahedron()],
    )
    .expect("all aggregate references are valid");

    let primitive = &asset.visual_mesh().primitives()[0];
    assert_eq!(
        primitive.positions()[1].map(f32::to_bits),
        [1.0, 0.0, 0.0].map(f32::to_bits)
    );
    assert_eq!(
        primitive.normals()[0].map(f32::to_bits),
        [0.0, 0.0, 1.0].map(f32::to_bits)
    );
    assert_eq!(
        primitive.tex_coord_sets()[0][2].map(f32::to_bits),
        [0.0, 1.0].map(f32::to_bits)
    );
    assert_eq!(primitive.indices(), [0, 1, 2]);
    assert_eq!(primitive.material(), Some(MaterialIndex::new(0)));

    let material = &asset.materials()[0];
    assert_eq!(
        material.base_color_factor().map(f32::to_bits),
        [0.25, 0.5, 0.75, 1.0].map(f32::to_bits)
    );
    assert_eq!(material.metallic_factor().to_bits(), 0.2_f32.to_bits());
    assert_eq!(material.roughness_factor().to_bits(), 0.8_f32.to_bits());
    assert_eq!(
        material.base_color_texture(),
        Some(TextureBinding::new(TextureIndex::new(0), 0))
    );

    let texture = &asset.textures()[0];
    assert_eq!(texture.image().dimensions(), [1, 1]);
    assert_eq!(texture.image().rgba8_srgb(), [64, 128, 255, 192]);
    assert_eq!(
        texture.sampler().minification(),
        Some(MinificationFilter::LinearMipmapLinear)
    );
    assert_eq!(texture.sampler().wrap_v(), WrapMode::ClampToEdge);
    assert_eq!(asset.convex_parts()[0].vertices().len(), 4);
}

#[test]
fn visual_mesh_rejects_empty_and_inconsistent_vertex_streams() {
    assert_eq!(
        VisualMesh::try_new(vec![]).unwrap_err(),
        InvalidVisualMeshError::EmptyPrimitives
    );

    let valid_positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let valid_normals = vec![[0.0, 0.0, 1.0]; 3];

    assert_eq!(
        VisualPrimitive::try_new(vec![], vec![], vec![], vec![0, 1, 2], None).unwrap_err(),
        InvalidVisualPrimitiveError::TooFewPositions { count: 0 }
    );
    assert_eq!(
        VisualPrimitive::try_new(
            valid_positions.clone(),
            vec![[0.0, 0.0, 1.0]; 2],
            vec![],
            vec![0, 1, 2],
            None,
        )
        .unwrap_err(),
        InvalidVisualPrimitiveError::NormalCountMismatch {
            positions: 3,
            normals: 2,
        }
    );
    assert_eq!(
        VisualPrimitive::try_new(
            vec![[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            valid_normals,
            vec![],
            vec![0, 1, 2],
            None,
        )
        .unwrap_err(),
        InvalidVisualPrimitiveError::NonFinitePosition {
            vertex: 0,
            component: 0,
        }
    );
}

#[test]
fn visual_primitive_rejects_unusable_normals_and_texture_coordinates() {
    let valid_positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let valid_normals = vec![[0.0, 0.0, 1.0]; 3];

    assert_eq!(
        VisualPrimitive::try_new(
            valid_positions.clone(),
            vec![[0.0, f32::INFINITY, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]],
            vec![],
            vec![0, 1, 2],
            None,
        )
        .unwrap_err(),
        InvalidVisualPrimitiveError::NonFiniteNormal {
            vertex: 0,
            component: 1,
        }
    );
    assert_eq!(
        VisualPrimitive::try_new(
            valid_positions.clone(),
            vec![[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]],
            vec![],
            vec![0, 1, 2],
            None,
        )
        .unwrap_err(),
        InvalidVisualPrimitiveError::ZeroNormal { vertex: 0 }
    );
    assert_eq!(
        VisualPrimitive::try_new(
            valid_positions.clone(),
            valid_normals.clone(),
            vec![vec![[0.0, 0.0]; 2]],
            vec![0, 1, 2],
            None,
        )
        .unwrap_err(),
        InvalidVisualPrimitiveError::TexCoordCountMismatch {
            set: 0,
            positions: 3,
            tex_coords: 2,
        }
    );
    assert_eq!(
        VisualPrimitive::try_new(
            valid_positions.clone(),
            valid_normals.clone(),
            vec![vec![[0.0, 0.0], [f32::NAN, 0.0], [0.0, 1.0]]],
            vec![0, 1, 2],
            None,
        )
        .unwrap_err(),
        InvalidVisualPrimitiveError::NonFiniteTexCoord {
            set: 0,
            vertex: 1,
            component: 0,
        }
    );
}

#[test]
fn visual_primitive_rejects_malformed_triangle_indices() {
    let valid_positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let valid_normals = vec![[0.0, 0.0, 1.0]; 3];

    assert_eq!(
        VisualPrimitive::try_new(
            valid_positions.clone(),
            valid_normals.clone(),
            vec![],
            vec![],
            None,
        )
        .unwrap_err(),
        InvalidVisualPrimitiveError::EmptyIndices
    );
    assert_eq!(
        VisualPrimitive::try_new(
            valid_positions.clone(),
            valid_normals.clone(),
            vec![],
            vec![0, 1],
            None,
        )
        .unwrap_err(),
        InvalidVisualPrimitiveError::IndexCountNotMultipleOfThree { count: 2 }
    );
    assert_eq!(
        VisualPrimitive::try_new(valid_positions, valid_normals, vec![], vec![0, 1, 3], None,)
            .unwrap_err(),
        InvalidVisualPrimitiveError::IndexOutOfBounds {
            index_offset: 2,
            index: 3,
            vertex_count: 3,
        }
    );
}

#[test]
fn pbr_material_rejects_nonfinite_and_out_of_range_factors() {
    assert_eq!(
        PbrMaterial::try_new([0.0, 1.1, 0.0, 1.0], 0.0, 1.0, None).unwrap_err(),
        InvalidPbrMaterialError::InvalidBaseColorComponent { component: 1 }
    );
    assert_eq!(
        PbrMaterial::try_new([1.0; 4], f32::NAN, 1.0, None).unwrap_err(),
        InvalidPbrMaterialError::InvalidMetallicFactor
    );
    assert_eq!(
        PbrMaterial::try_new([1.0; 4], 0.0, -0.1, None).unwrap_err(),
        InvalidPbrMaterialError::InvalidRoughnessFactor
    );
}

#[test]
fn texture_image_checks_dimensions_overflow_and_exact_rgba_length() {
    assert_eq!(
        TextureImage::try_new(0, 1, vec![]).unwrap_err(),
        InvalidTextureImageError::ZeroWidth
    );
    assert_eq!(
        TextureImage::try_new(1, 0, vec![]).unwrap_err(),
        InvalidTextureImageError::ZeroHeight
    );
    assert_eq!(
        TextureImage::try_new(u32::MAX, u32::MAX, vec![]).unwrap_err(),
        InvalidTextureImageError::PixelCountOverflow
    );
    assert_eq!(
        TextureImage::try_new(2, 1, vec![0; 7]).unwrap_err(),
        InvalidTextureImageError::PixelLengthMismatch {
            expected: 8,
            actual: 7,
        }
    );
}

#[test]
fn convex_parts_reject_invalid_surface_data_without_claiming_to_prove_convexity() {
    assert_eq!(
        ConvexPart::try_new(vec![[0.0; 3]; 3], vec![0, 1, 2]).unwrap_err(),
        InvalidConvexPartError::TooFewVertices { count: 3 }
    );
    assert_eq!(
        ConvexPart::try_new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, f32::INFINITY, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![0, 1, 2],
        )
        .unwrap_err(),
        InvalidConvexPartError::NonFiniteVertex {
            vertex: 1,
            component: 1,
        }
    );
    assert_eq!(
        ConvexPart::try_new(vec![[0.0; 3]; 4], vec![0, 1]).unwrap_err(),
        InvalidConvexPartError::IndexCountNotMultipleOfThree { count: 2 }
    );
    assert_eq!(
        ConvexPart::try_new(vec![[0.0; 3]; 4], vec![]).unwrap_err(),
        InvalidConvexPartError::EmptyIndices
    );
    assert_eq!(
        ConvexPart::try_new(vec![[0.0; 3]; 4], vec![0, 1, 4]).unwrap_err(),
        InvalidConvexPartError::IndexOutOfBounds {
            index_offset: 2,
            index: 4,
            vertex_count: 4,
        }
    );
}

#[test]
fn normalized_asset_rejects_dangling_cross_references() {
    assert_eq!(
        NormalizedAsset::try_new(
            VisualMesh::try_new(vec![triangle(Some(MaterialIndex::new(0)), false)]).unwrap(),
            vec![],
            vec![],
            vec![tetrahedron()],
        )
        .unwrap_err(),
        InvalidNormalizedAssetError::MaterialOutOfBounds {
            primitive: 0,
            material: MaterialIndex::new(0),
            material_count: 0,
        }
    );

    assert_eq!(
        NormalizedAsset::try_new(
            VisualMesh::try_new(vec![triangle(Some(MaterialIndex::new(0)), true)]).unwrap(),
            vec![textured_material(TextureBinding::new(
                TextureIndex::new(0),
                0,
            ))],
            vec![],
            vec![tetrahedron()],
        )
        .unwrap_err(),
        InvalidNormalizedAssetError::TextureOutOfBounds {
            material: 0,
            texture: TextureIndex::new(0),
            texture_count: 0,
        }
    );

    assert_eq!(
        NormalizedAsset::try_new(
            VisualMesh::try_new(vec![triangle(Some(MaterialIndex::new(0)), false)]).unwrap(),
            vec![textured_material(TextureBinding::new(
                TextureIndex::new(0),
                0,
            ))],
            vec![texture()],
            vec![tetrahedron()],
        )
        .unwrap_err(),
        InvalidNormalizedAssetError::MissingTexCoordSet {
            primitive: 0,
            material: 0,
            set: 0,
            available: 0,
        }
    );

    assert_eq!(
        NormalizedAsset::try_new(
            VisualMesh::try_new(vec![triangle(None, false)]).unwrap(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap_err(),
        InvalidNormalizedAssetError::EmptyConvexParts
    );
}

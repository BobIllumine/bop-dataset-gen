use crate::{ConvexPart, InvalidNormalizedAssetError, PbrMaterial, Texture, VisualMesh};

/// Fully checked renderer-neutral visual and collision representation.
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedAsset {
    visual_mesh: VisualMesh,
    materials: Vec<PbrMaterial>,
    textures: Vec<Texture>,
    convex_parts: Vec<ConvexPart>,
}

impl NormalizedAsset {
    /// Creates a normalized asset and validates all cross-table references.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidNormalizedAssetError`] for missing collision data or
    /// dangling material, texture, or texture-coordinate references.
    pub fn try_new(
        visual_mesh: VisualMesh,
        materials: Vec<PbrMaterial>,
        textures: Vec<Texture>,
        convex_parts: Vec<ConvexPart>,
    ) -> Result<Self, InvalidNormalizedAssetError> {
        if convex_parts.is_empty() {
            return Err(InvalidNormalizedAssetError::EmptyConvexParts);
        }

        for (primitive_index, primitive) in visual_mesh.primitives().iter().enumerate() {
            if let Some(material) = primitive.material() {
                let material_index = usize::try_from(material.get()).unwrap_or(usize::MAX);
                if material_index >= materials.len() {
                    return Err(InvalidNormalizedAssetError::MaterialOutOfBounds {
                        primitive: primitive_index,
                        material,
                        material_count: materials.len(),
                    });
                }
            }
        }

        for (material_index, material) in materials.iter().enumerate() {
            if let Some(binding) = material.base_color_texture() {
                let texture_index = usize::try_from(binding.texture().get()).unwrap_or(usize::MAX);
                if texture_index >= textures.len() {
                    return Err(InvalidNormalizedAssetError::TextureOutOfBounds {
                        material: material_index,
                        texture: binding.texture(),
                        texture_count: textures.len(),
                    });
                }
            }
        }

        for (primitive_index, primitive) in visual_mesh.primitives().iter().enumerate() {
            let Some(material) = primitive.material() else {
                continue;
            };
            let material_index = usize::try_from(material.get()).unwrap_or(usize::MAX);
            let Some(binding) = materials[material_index].base_color_texture() else {
                continue;
            };
            let set_index = usize::try_from(binding.tex_coord_set()).unwrap_or(usize::MAX);
            if set_index >= primitive.tex_coord_sets().len() {
                return Err(InvalidNormalizedAssetError::MissingTexCoordSet {
                    primitive: primitive_index,
                    material: material_index,
                    set: binding.tex_coord_set(),
                    available: primitive.tex_coord_sets().len(),
                });
            }
        }

        Ok(Self {
            visual_mesh,
            materials,
            textures,
            convex_parts,
        })
    }

    /// Returns normalized visual geometry.
    #[must_use]
    pub const fn visual_mesh(&self) -> &VisualMesh {
        &self.visual_mesh
    }

    /// Returns materials in stable table order.
    #[must_use]
    pub fn materials(&self) -> &[PbrMaterial] {
        &self.materials
    }

    /// Returns decoded textures in stable table order.
    #[must_use]
    pub fn textures(&self) -> &[Texture] {
        &self.textures
    }

    /// Returns collision parts in stable decomposition order.
    #[must_use]
    pub fn convex_parts(&self) -> &[ConvexPart] {
        &self.convex_parts
    }
}

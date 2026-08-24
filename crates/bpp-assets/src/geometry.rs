use crate::{
    InvalidConvexPartError, InvalidVisualMeshError, InvalidVisualPrimitiveError, MaterialIndex,
};

/// Checked indexed triangle primitive in metres in its owning asset frame.
#[derive(Clone, Debug, PartialEq)]
pub struct VisualPrimitive {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    tex_coord_sets: Vec<Vec<[f32; 2]>>,
    indices: Vec<u32>,
    material: Option<MaterialIndex>,
}

pub(crate) struct VisualPrimitiveParts {
    pub(crate) positions: Vec<[f32; 3]>,
    pub(crate) normals: Vec<[f32; 3]>,
    pub(crate) tex_coord_sets: Vec<Vec<[f32; 2]>>,
    pub(crate) indices: Vec<u32>,
    pub(crate) material: Option<MaterialIndex>,
}

impl VisualPrimitive {
    /// Creates a checked visual primitive and normalizes every input normal.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidVisualPrimitiveError`] for malformed, non-finite, or
    /// mutually inconsistent vertex and index streams.
    pub fn try_new(
        positions: Vec<[f32; 3]>,
        mut normals: Vec<[f32; 3]>,
        tex_coord_sets: Vec<Vec<[f32; 2]>>,
        indices: Vec<u32>,
        material: Option<MaterialIndex>,
    ) -> Result<Self, InvalidVisualPrimitiveError> {
        if positions.len() < 3 {
            return Err(InvalidVisualPrimitiveError::TooFewPositions {
                count: positions.len(),
            });
        }
        if normals.len() != positions.len() {
            return Err(InvalidVisualPrimitiveError::NormalCountMismatch {
                positions: positions.len(),
                normals: normals.len(),
            });
        }
        validate_finite_positions(&positions)?;
        normalize_normals(&mut normals)?;
        validate_tex_coords(&tex_coord_sets, positions.len())?;
        validate_visual_indices(&indices, positions.len())?;
        Ok(Self {
            positions,
            normals,
            tex_coord_sets,
            indices,
            material,
        })
    }

    /// Returns vertex positions in metres in the owning asset frame.
    #[must_use]
    pub fn positions(&self) -> &[[f32; 3]] {
        &self.positions
    }

    /// Returns normalized vertex normals in the owning asset frame.
    #[must_use]
    pub fn normals(&self) -> &[[f32; 3]] {
        &self.normals
    }

    /// Returns all texture-coordinate sets, each aligned with the vertex streams.
    #[must_use]
    pub fn tex_coord_sets(&self) -> &[Vec<[f32; 2]>] {
        &self.tex_coord_sets
    }

    /// Returns triangle-list vertex indices.
    #[must_use]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// Returns the optional material-table index.
    #[must_use]
    pub const fn material(&self) -> Option<MaterialIndex> {
        self.material
    }

    pub(crate) fn into_parts(self) -> VisualPrimitiveParts {
        VisualPrimitiveParts {
            positions: self.positions,
            normals: self.normals,
            tex_coord_sets: self.tex_coord_sets,
            indices: self.indices,
            material: self.material,
        }
    }

    pub(crate) fn try_new_preserving_normals(
        positions: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
        tex_coord_sets: Vec<Vec<[f32; 2]>>,
        indices: Vec<u32>,
        material: Option<MaterialIndex>,
    ) -> Result<Self, InvalidVisualPrimitiveError> {
        if positions.len() < 3 {
            return Err(InvalidVisualPrimitiveError::TooFewPositions {
                count: positions.len(),
            });
        }
        if normals.len() != positions.len() {
            return Err(InvalidVisualPrimitiveError::NormalCountMismatch {
                positions: positions.len(),
                normals: normals.len(),
            });
        }
        validate_finite_positions(&positions)?;
        validate_usable_normals(&normals)?;
        validate_tex_coords(&tex_coord_sets, positions.len())?;
        validate_visual_indices(&indices, positions.len())?;
        Ok(Self {
            positions,
            normals,
            tex_coord_sets,
            indices,
            material,
        })
    }
}

/// Checked collection of one or more visual primitives.
#[derive(Clone, Debug, PartialEq)]
pub struct VisualMesh {
    primitives: Vec<VisualPrimitive>,
}

impl VisualMesh {
    /// Creates a nonempty visual mesh.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidVisualMeshError::EmptyPrimitives`] for an empty mesh.
    pub fn try_new(primitives: Vec<VisualPrimitive>) -> Result<Self, InvalidVisualMeshError> {
        if primitives.is_empty() {
            return Err(InvalidVisualMeshError::EmptyPrimitives);
        }
        Ok(Self { primitives })
    }

    /// Returns the visual primitives in stable source order.
    #[must_use]
    pub fn primitives(&self) -> &[VisualPrimitive] {
        &self.primitives
    }

    pub(crate) fn into_primitives(self) -> Vec<VisualPrimitive> {
        self.primitives
    }
}

/// Checked triangular surface intended to describe one convex collision part.
///
/// Construction validates basic surface integrity but does not mathematically
/// prove convexity. Convex-decomposition producers own that stronger guarantee.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvexPart {
    vertices: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

impl ConvexPart {
    /// Creates a checked convex-part surface.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidConvexPartError`] for insufficient or malformed
    /// surface data.
    pub fn try_new(
        vertices: Vec<[f32; 3]>,
        indices: Vec<u32>,
    ) -> Result<Self, InvalidConvexPartError> {
        if vertices.len() < 4 {
            return Err(InvalidConvexPartError::TooFewVertices {
                count: vertices.len(),
            });
        }
        for (vertex, value) in vertices.iter().enumerate() {
            for (component, scalar) in value.iter().enumerate() {
                if !scalar.is_finite() {
                    return Err(InvalidConvexPartError::NonFiniteVertex { vertex, component });
                }
            }
        }
        validate_convex_indices(&indices, vertices.len())?;
        Ok(Self { vertices, indices })
    }

    /// Returns collision vertices in canonical metres.
    #[must_use]
    pub fn vertices(&self) -> &[[f32; 3]] {
        &self.vertices
    }

    /// Returns triangle-list indices for the convex surface.
    #[must_use]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }
}

fn validate_finite_positions(positions: &[[f32; 3]]) -> Result<(), InvalidVisualPrimitiveError> {
    for (vertex, position) in positions.iter().enumerate() {
        for (component, scalar) in position.iter().enumerate() {
            if !scalar.is_finite() {
                return Err(InvalidVisualPrimitiveError::NonFinitePosition { vertex, component });
            }
        }
    }
    Ok(())
}

fn normalize_normals(normals: &mut [[f32; 3]]) -> Result<(), InvalidVisualPrimitiveError> {
    for (vertex, normal) in normals.iter_mut().enumerate() {
        let squared_length = validate_usable_normal(*normal, vertex)?;
        let length = squared_length.sqrt();
        for component in normal {
            *component /= length;
        }
    }
    Ok(())
}

fn validate_usable_normals(normals: &[[f32; 3]]) -> Result<(), InvalidVisualPrimitiveError> {
    for (vertex, normal) in normals.iter().copied().enumerate() {
        validate_usable_normal(normal, vertex)?;
    }
    Ok(())
}

fn validate_usable_normal(
    normal: [f32; 3],
    vertex: usize,
) -> Result<f32, InvalidVisualPrimitiveError> {
    for (component, scalar) in normal.iter().enumerate() {
        if !scalar.is_finite() {
            return Err(InvalidVisualPrimitiveError::NonFiniteNormal { vertex, component });
        }
    }
    let squared_length = normal.iter().map(|value| value * value).sum::<f32>();
    if squared_length == 0.0 || !squared_length.is_finite() {
        return Err(InvalidVisualPrimitiveError::ZeroNormal { vertex });
    }
    Ok(squared_length)
}

fn validate_tex_coords(
    tex_coord_sets: &[Vec<[f32; 2]>],
    position_count: usize,
) -> Result<(), InvalidVisualPrimitiveError> {
    for (set, tex_coords) in tex_coord_sets.iter().enumerate() {
        if tex_coords.len() != position_count {
            return Err(InvalidVisualPrimitiveError::TexCoordCountMismatch {
                set,
                positions: position_count,
                tex_coords: tex_coords.len(),
            });
        }
        for (vertex, tex_coord) in tex_coords.iter().enumerate() {
            for (component, scalar) in tex_coord.iter().enumerate() {
                if !scalar.is_finite() {
                    return Err(InvalidVisualPrimitiveError::NonFiniteTexCoord {
                        set,
                        vertex,
                        component,
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_visual_indices(
    indices: &[u32],
    vertex_count: usize,
) -> Result<(), InvalidVisualPrimitiveError> {
    if indices.is_empty() {
        return Err(InvalidVisualPrimitiveError::EmptyIndices);
    }
    if !indices.len().is_multiple_of(3) {
        return Err(InvalidVisualPrimitiveError::IndexCountNotMultipleOfThree {
            count: indices.len(),
        });
    }
    for (index_offset, &index) in indices.iter().enumerate() {
        if usize::try_from(index).map_or(true, |value| value >= vertex_count) {
            return Err(InvalidVisualPrimitiveError::IndexOutOfBounds {
                index_offset,
                index,
                vertex_count,
            });
        }
    }
    Ok(())
}

fn validate_convex_indices(
    indices: &[u32],
    vertex_count: usize,
) -> Result<(), InvalidConvexPartError> {
    if indices.is_empty() {
        return Err(InvalidConvexPartError::EmptyIndices);
    }
    if !indices.len().is_multiple_of(3) {
        return Err(InvalidConvexPartError::IndexCountNotMultipleOfThree {
            count: indices.len(),
        });
    }
    for (index_offset, &index) in indices.iter().enumerate() {
        if usize::try_from(index).map_or(true, |value| value >= vertex_count) {
            return Err(InvalidConvexPartError::IndexOutOfBounds {
                index_offset,
                index,
                vertex_count,
            });
        }
    }
    Ok(())
}

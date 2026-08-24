use bpp_core::{Meters, RigidTransform};
use glam::{Quat, Vec3};
use thiserror::Error;

use crate::{
    InvalidVisualMeshError, InvalidVisualPrimitiveError, LoadedVisualAsset, PbrMaterial, Texture,
    VisualMesh, VisualPrimitive,
};

/// Visual asset whose geometry is explicitly expressed in canonical Z-up metres.
#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalVisualAsset {
    visual_mesh: VisualMesh,
    materials: Vec<PbrMaterial>,
    textures: Vec<Texture>,
}

impl CanonicalVisualAsset {
    /// Returns canonical Z-up visual geometry in metres.
    #[must_use]
    pub const fn visual_mesh(&self) -> &VisualMesh {
        &self.visual_mesh
    }

    /// Returns materials in stable source-table order.
    #[must_use]
    pub fn materials(&self) -> &[PbrMaterial] {
        &self.materials
    }

    /// Returns textures in stable source-table order.
    #[must_use]
    pub fn textures(&self) -> &[Texture] {
        &self.textures
    }
}

/// Error returned when source-model geometry cannot be represented canonically.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CanonicalizeVisualAssetError {
    /// A transformed position overflowed finite metre representation.
    #[error("canonical primitive {primitive} vertex {vertex} is not representable in metres")]
    PositionOutOfRange {
        /// Primitive table index.
        primitive: usize,
        /// Vertex index within the primitive.
        vertex: usize,
    },
    /// Reconstructing a transformed primitive violated mesh invariants.
    #[error("canonical primitive {primitive} is invalid: {source}")]
    InvalidPrimitive {
        /// Primitive table index.
        primitive: usize,
        /// Violated primitive invariant.
        source: InvalidVisualPrimitiveError,
    },
    /// Reconstructing the transformed mesh violated aggregate invariants.
    #[error("canonical visual mesh is invalid: {0}")]
    InvalidMesh(InvalidVisualMeshError),
}

/// Converts a loaded source-frame asset into the canonical Z-up stage.
///
/// Geometry receives the explicit rigid transform while material and texture
/// tables move unchanged into the canonical asset. No implicit recentering or
/// axis inference occurs.
///
/// # Errors
///
/// Returns [`CanonicalizeVisualAssetError`] when transformed geometry cannot
/// be represented by the checked visual-mesh contract.
pub fn canonicalize_visual_asset(
    source: LoadedVisualAsset,
    canonical_from_model: RigidTransform,
) -> Result<CanonicalVisualAsset, CanonicalizeVisualAssetError> {
    let (visual_mesh, materials, textures) = source.into_parts();
    Ok(CanonicalVisualAsset {
        visual_mesh: canonicalize_visual_mesh(visual_mesh, canonical_from_model)?,
        materials,
        textures,
    })
}

/// Applies an explicit rigid model-to-canonical transform to visual geometry.
///
/// Input positions are already measured in metres. The transform's rotation
/// and metre translation are applied exactly once; no recentering, scaling,
/// axis inference, or bounding-box alignment occurs. Normals receive rotation
/// only. Primitive order, indices, UVs, and material indices are preserved.
///
/// # Errors
///
/// Returns [`CanonicalizeVisualAssetError`] if a transformed position is not
/// finite or the reconstructed checked mesh violates an invariant.
fn canonicalize_visual_mesh(
    source: VisualMesh,
    canonical_from_model: RigidTransform,
) -> Result<VisualMesh, CanonicalizeVisualAssetError> {
    let rotation = Quat::from_array(canonical_from_model.rotation_xyzw());
    let source_primitives = source.into_primitives();
    let mut canonical_primitives = Vec::with_capacity(source_primitives.len());
    for (primitive_index, primitive) in source_primitives.into_iter().enumerate() {
        let parts = primitive.into_parts();
        let mut positions = Vec::with_capacity(parts.positions.len());
        for (vertex, position) in parts.positions.into_iter().enumerate() {
            let point = position.map(Meters::new);
            let [Some(x), Some(y), Some(z)] = point else {
                return Err(CanonicalizeVisualAssetError::PositionOutOfRange {
                    primitive: primitive_index,
                    vertex,
                });
            };
            let transformed = canonical_from_model.transform_point([x, y, z]).ok_or(
                CanonicalizeVisualAssetError::PositionOutOfRange {
                    primitive: primitive_index,
                    vertex,
                },
            )?;
            positions.push(transformed.map(Meters::get));
        }
        let normals = parts
            .normals
            .into_iter()
            .map(|normal| (rotation * Vec3::from_array(normal)).to_array())
            .collect();
        let canonical = VisualPrimitive::try_new_preserving_normals(
            positions,
            normals,
            parts.tex_coord_sets,
            parts.indices,
            parts.material,
        )
        .map_err(|source| CanonicalizeVisualAssetError::InvalidPrimitive {
            primitive: primitive_index,
            source,
        })?;
        canonical_primitives.push(canonical);
    }
    VisualMesh::try_new(canonical_primitives).map_err(CanonicalizeVisualAssetError::InvalidMesh)
}

use std::panic::{AssertUnwindSafe, catch_unwind};

use bpp_core::VhacdParameters;
use parry3d::{
    math::Vector,
    transformation::{
        try_convex_hull,
        vhacd::{VHACD, VHACDParameters},
    },
};
use thiserror::Error;

use crate::{CanonicalVisualAsset, ConvexPart, InvalidConvexPartError};

const MAX_VOXEL_BUDGET: u32 = 2_097_152;
const MAX_CONVEX_HULLS: u32 = 1_024;
const MAX_DOWNSAMPLING: u32 = 256;

/// Bounded V-HACD input whose excess is reported before entering Parry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum VhacdResourceParameter {
    /// Total voxel budget, converted to longest-axis subdivisions.
    VoxelBudget,
    /// Maximum requested convex hull count.
    MaxConvexHulls,
    /// Cutting-plane search downsampling factor.
    PlaneDownsampling,
    /// Convex-hull generation downsampling factor.
    ConvexHullDownsampling,
}

/// Error returned when canonical visual geometry cannot produce checked convex parts.
#[derive(Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum ConvexDecompositionError {
    /// A V-HACD resource parameter exceeds the adapter's safe operating bound.
    #[error("V-HACD {parameter:?} value {requested} exceeds supported maximum {maximum}")]
    ResourceLimitExceeded {
        /// Parameter whose value is unsupported.
        parameter: VhacdResourceParameter,
        /// Configured value.
        requested: u32,
        /// Largest accepted value.
        maximum: u32,
    },
    /// Flattened vertex or index arithmetic exceeded the supported `u32` domain.
    #[error("canonical mesh exceeds the supported u32 index domain")]
    IndexDomainOverflow,
    /// Parry's external V-HACD implementation panicked.
    #[error("Parry V-HACD panicked")]
    ExternalAlgorithmPanicked,
    /// V-HACD returned no collision hulls.
    #[error("Parry V-HACD produced no convex hulls")]
    EmptyOutput,
    /// V-HACD returned more collision hulls than configured.
    #[error("Parry V-HACD produced {actual} hulls, exceeding configured maximum {maximum}")]
    HullLimitExceeded {
        /// Number of hulls returned by Parry.
        actual: usize,
        /// Configured maximum number of hulls.
        maximum: u32,
    },
    /// Parry could not construct a convex hull while coalescing excess leaves.
    #[error("Parry could not coalesce convex hull group {part}")]
    HullConstructionFailed {
        /// Stable zero-based coalesced output index.
        part: usize,
    },
    /// One external hull violated the project's checked convex-part contract.
    #[error("generated convex part {part} is invalid: {source}")]
    InvalidPart {
        /// Stable zero-based Parry output index.
        part: usize,
        /// Violated project geometry invariant.
        source: InvalidConvexPartError,
    },
}

/// Decomposes canonical Z-up metre geometry into checked collision hulls.
///
/// V1 uses Parry's faster voxel-derived hulls and preserves Parry's output
/// order. Repeated calls with the same inputs and pinned build are expected to
/// agree, but this API does not promise bitwise equality across dependency,
/// platform, or toolchain changes. Parry types remain private to this adapter.
/// Before invoking Parry, the adapter limits total voxel budget to 2,097,152,
/// requested hulls to 1,024, and either downsampling factor to 256. These
/// bounds prevent uncatchable allocation failure and overflow in Parry's
/// internal `u32` arithmetic.
///
/// # Errors
///
/// Returns [`ConvexDecompositionError`] for an unsupported resource request,
/// checked index overflow, a contained external-algorithm panic, empty or
/// over-limit output, or any generated hull that violates [`ConvexPart`]
/// invariants.
pub fn generate_convex_parts(
    asset: &CanonicalVisualAsset,
    parameters: VhacdParameters,
) -> Result<Vec<ConvexPart>, ConvexDecompositionError> {
    validate_parameters(parameters)?;
    let (vertices, indices) = flatten(asset)?;
    let parry_parameters = VHACDParameters {
        resolution: axis_resolution(parameters.resolution()),
        max_convex_hulls: parameters.max_convex_hulls(),
        concavity: parameters.concavity(),
        plane_downsampling: parameters.plane_downsampling(),
        convex_hull_downsampling: parameters.convex_hull_downsampling(),
        ..VHACDParameters::default()
    };
    let hulls = catch_unwind(AssertUnwindSafe(|| {
        VHACD::decompose(&parry_parameters, &vertices, &indices, false)
            .compute_convex_hulls(parameters.convex_hull_downsampling())
    }))
    .map_err(|_| ConvexDecompositionError::ExternalAlgorithmPanicked)?;

    let hulls = validate_raw_hulls(hulls)?;
    let maximum = usize::try_from(parameters.max_convex_hulls())
        .map_err(|_| ConvexDecompositionError::IndexDomainOverflow)?;
    let hulls = if hulls.len() > maximum {
        coalesce_hulls(hulls, maximum)?
    } else {
        hulls
    };
    if hulls.len() > maximum {
        return Err(ConvexDecompositionError::HullLimitExceeded {
            actual: hulls.len(),
            maximum: parameters.max_convex_hulls(),
        });
    }

    hulls
        .into_iter()
        .enumerate()
        .map(|(part, (vertices, triangles))| {
            let vertices = vertices
                .into_iter()
                .map(|vertex| vertex.to_array())
                .collect();
            let indices = triangles.into_iter().flatten().collect();
            ConvexPart::try_new(vertices, indices)
                .map_err(|source| ConvexDecompositionError::InvalidPart { part, source })
        })
        .collect()
}

fn validate_parameters(parameters: VhacdParameters) -> Result<(), ConvexDecompositionError> {
    check_limit(
        VhacdResourceParameter::VoxelBudget,
        parameters.resolution(),
        MAX_VOXEL_BUDGET,
    )?;
    check_limit(
        VhacdResourceParameter::MaxConvexHulls,
        parameters.max_convex_hulls(),
        MAX_CONVEX_HULLS,
    )?;
    check_limit(
        VhacdResourceParameter::PlaneDownsampling,
        parameters.plane_downsampling(),
        MAX_DOWNSAMPLING,
    )?;
    check_limit(
        VhacdResourceParameter::ConvexHullDownsampling,
        parameters.convex_hull_downsampling(),
        MAX_DOWNSAMPLING,
    )
}

fn check_limit(
    parameter: VhacdResourceParameter,
    requested: u32,
    maximum: u32,
) -> Result<(), ConvexDecompositionError> {
    if requested > maximum {
        Err(ConvexDecompositionError::ResourceLimitExceeded {
            parameter,
            requested,
            maximum,
        })
    } else {
        Ok(())
    }
}

type ParryHull = (Vec<Vector>, Vec<[u32; 3]>);

fn validate_raw_hulls(hulls: Vec<ParryHull>) -> Result<Vec<ParryHull>, ConvexDecompositionError> {
    let mut checked = Vec::with_capacity(hulls.len());
    for (part, hull) in hulls.into_iter().enumerate() {
        if hull.0.is_empty() && hull.1.is_empty() {
            continue;
        }
        let vertices = hull.0.iter().map(Vector::to_array).collect();
        let indices = hull.1.iter().flatten().copied().collect();
        ConvexPart::try_new(vertices, indices)
            .map_err(|source| ConvexDecompositionError::InvalidPart { part, source })?;
        checked.push(hull);
    }
    if checked.is_empty() {
        Err(ConvexDecompositionError::EmptyOutput)
    } else {
        Ok(checked)
    }
}

fn coalesce_hulls(
    hulls: Vec<ParryHull>,
    maximum: usize,
) -> Result<Vec<ParryHull>, ConvexDecompositionError> {
    let total = hulls.len();
    let group_size = total / maximum;
    let larger_groups = total % maximum;
    let mut hulls = hulls.into_iter();
    let mut coalesced = Vec::with_capacity(maximum);
    for part in 0..maximum {
        let count = group_size + usize::from(part < larger_groups);
        let group = hulls.by_ref().take(count).collect::<Vec<_>>();
        if group.len() == 1 {
            coalesced.push(group.into_iter().next().expect("one hull was collected"));
            continue;
        }
        let vertices = group
            .into_iter()
            .flat_map(|(vertices, _)| vertices)
            .collect::<Vec<_>>();
        let hull = catch_unwind(AssertUnwindSafe(|| try_convex_hull(&vertices)))
            .map_err(|_| ConvexDecompositionError::ExternalAlgorithmPanicked)?
            .map_err(|_| ConvexDecompositionError::HullConstructionFailed { part })?;
        coalesced.push(hull);
    }
    Ok(coalesced)
}

fn axis_resolution(voxel_budget: u32) -> u32 {
    const MAX_CUBE_ROOT_U32: u32 = 1_626;
    let mut lower = 1_u32;
    let mut upper = voxel_budget.min(MAX_CUBE_ROOT_U32);
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        if u64::from(middle).pow(3) >= u64::from(voxel_budget) {
            upper = middle;
        } else {
            lower = middle + 1;
        }
    }
    lower
}

fn flatten(
    asset: &CanonicalVisualAsset,
) -> Result<(Vec<Vector>, Vec<[u32; 3]>), ConvexDecompositionError> {
    let primitives = asset.visual_mesh().primitives();
    let vertex_count = primitives.iter().try_fold(0_usize, |total, primitive| {
        total.checked_add(primitive.positions().len())
    });
    let index_count = primitives.iter().try_fold(0_usize, |total, primitive| {
        total.checked_add(primitive.indices().len())
    });
    let vertex_count = vertex_count.ok_or(ConvexDecompositionError::IndexDomainOverflow)?;
    let index_count = index_count.ok_or(ConvexDecompositionError::IndexDomainOverflow)?;
    u32::try_from(vertex_count).map_err(|_| ConvexDecompositionError::IndexDomainOverflow)?;

    let mut vertices = Vec::with_capacity(vertex_count);
    let mut triangles = Vec::with_capacity(index_count / 3);
    for primitive in primitives {
        let base = u32::try_from(vertices.len())
            .map_err(|_| ConvexDecompositionError::IndexDomainOverflow)?;
        vertices.extend(
            primitive
                .positions()
                .iter()
                .copied()
                .map(Vector::from_array),
        );
        for triangle in primitive.indices().chunks_exact(3) {
            triangles.push([
                base.checked_add(triangle[0])
                    .ok_or(ConvexDecompositionError::IndexDomainOverflow)?,
                base.checked_add(triangle[1])
                    .ok_or(ConvexDecompositionError::IndexDomainOverflow)?,
                base.checked_add(triangle[2])
                    .ok_or(ConvexDecompositionError::IndexDomainOverflow)?,
            ]);
        }
    }
    Ok((vertices, triangles))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tetrahedron() -> ParryHull {
        (
            vec![
                Vector::new(0.0, 0.0, 0.0),
                Vector::new(1.0, 0.0, 0.0),
                Vector::new(0.0, 1.0, 0.0),
                Vector::new(0.0, 0.0, 1.0),
            ],
            vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]],
        )
    }

    #[test]
    fn total_voxel_budget_uses_integer_ceiling_cube_root() {
        assert_eq!(axis_resolution(1), 1);
        assert_eq!(axis_resolution(8), 2);
        assert_eq!(axis_resolution(9), 3);
        assert_eq!(axis_resolution(100_000), 47);
        assert_eq!(axis_resolution(MAX_VOXEL_BUDGET), 128);
    }

    #[test]
    fn fully_empty_external_leaves_are_removed_before_checked_output() {
        let prepared = validate_raw_hulls(vec![(Vec::new(), Vec::new()), tetrahedron()])
            .expect("one checked nonempty leaf remains");

        assert_eq!(prepared, vec![tetrahedron()]);
    }

    #[test]
    fn partially_empty_external_leaf_is_rejected_before_coalescing() {
        let (vertices, _) = tetrahedron();

        assert_eq!(
            validate_raw_hulls(vec![(vertices, Vec::new())]),
            Err(ConvexDecompositionError::InvalidPart {
                part: 0,
                source: InvalidConvexPartError::EmptyIndices,
            })
        );
    }

    #[test]
    fn out_of_bounds_external_leaf_is_rejected_before_coalescing() {
        let (vertices, mut triangles) = tetrahedron();
        triangles[0][1] = 99;

        assert_eq!(
            validate_raw_hulls(vec![(vertices, triangles)]),
            Err(ConvexDecompositionError::InvalidPart {
                part: 0,
                source: InvalidConvexPartError::IndexOutOfBounds {
                    index_offset: 1,
                    index: 99,
                    vertex_count: 4,
                },
            })
        );
    }
}

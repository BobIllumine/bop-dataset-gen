use std::panic::{AssertUnwindSafe, catch_unwind};

use bop_assets::ConvexPart;
use rapier3d::{
    parry::{
        mass_properties::MassProperties,
        shape::{ConvexPolyhedron, Shape},
    },
    prelude::{SharedShape, Vector},
};
use thiserror::Error;

/// An opaque collision asset whose indexed convex geometry has been prepared once.
#[derive(Debug)]
pub struct PreparedPhysicsAsset {
    shapes: Vec<SharedShape>,
    unit_density_mass_properties: MassProperties,
}

impl PreparedPhysicsAsset {
    /// Prepares shareable volumetric convex shapes from indexed source parts.
    ///
    /// Only vertices referenced by each part's triangle indices contribute to
    /// its hull. Repeated references are accepted.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsAssetPreparationError::EmptyParts`] for an empty asset,
    /// [`PhysicsAssetPreparationError::InvalidIndexedSurface`] when a referenced
    /// point cloud is not volumetric, or
    /// [`PhysicsAssetPreparationError::HullAlgorithmPanicked`] when the external
    /// hull implementation panics.
    pub fn try_new(parts: &[ConvexPart]) -> Result<Self, PhysicsAssetPreparationError> {
        if parts.is_empty() {
            return Err(PhysicsAssetPreparationError::EmptyParts);
        }

        let shapes = parts
            .iter()
            .enumerate()
            .map(|(part_index, part)| {
                prepare_part_with(part_index, part, ConvexPolyhedron::from_convex_hull)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let unit_density_mass_properties =
            shapes.iter().map(|shape| shape.mass_properties(1.0)).sum();

        Ok(Self {
            shapes,
            unit_density_mass_properties,
        })
    }

    /// Returns the number of prepared convex parts in source order.
    #[must_use]
    pub fn part_count(&self) -> usize {
        self.shapes.len()
    }

    pub(crate) fn shapes(&self) -> &[SharedShape] {
        &self.shapes
    }

    pub(crate) fn mass_properties(&self, mass: f32) -> MassProperties {
        let mut properties = self.unit_density_mass_properties;
        properties.set_mass(mass, true);
        properties
    }
}

/// Error returned when indexed convex geometry cannot be prepared for physics.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PhysicsAssetPreparationError {
    /// The asset contains no convex collision parts.
    #[error("physics asset contains no convex collision parts")]
    EmptyParts,
    /// A part's referenced vertices do not form a finite volumetric convex hull.
    #[error("physics asset convex part {part_index} is not a volumetric indexed surface")]
    InvalidIndexedSurface {
        /// The zero-based source-order index of the rejected convex part.
        part_index: usize,
    },
    /// The external convex-hull algorithm panicked while preparing a part.
    #[error("physics asset convex part {part_index} triggered a hull algorithm panic")]
    HullAlgorithmPanicked {
        /// The zero-based source-order index of the rejected convex part.
        part_index: usize,
    },
}

fn prepare_part_with<F>(
    part_index: usize,
    part: &ConvexPart,
    hull_builder: F,
) -> Result<SharedShape, PhysicsAssetPreparationError>
where
    F: FnOnce(&[Vector]) -> Option<ConvexPolyhedron>,
{
    let referenced_vertices = part
        .indices()
        .iter()
        .map(|&index| {
            let [x, y, z] = part.vertices()[index as usize];
            Vector::new(x, y, z)
        })
        .collect::<Vec<_>>();
    let hull = catch_unwind(AssertUnwindSafe(|| hull_builder(&referenced_vertices)))
        .map_err(|_| PhysicsAssetPreparationError::HullAlgorithmPanicked { part_index })?
        .filter(|hull| {
            let mass = hull.mass_properties(1.0).mass();
            mass.is_finite() && mass > 0.0
        })
        .ok_or(PhysicsAssetPreparationError::InvalidIndexedSurface { part_index })?;

    Ok(SharedShape::new(hull))
}

#[cfg(test)]
mod tests {
    use bop_assets::ConvexPart;

    use super::{PhysicsAssetPreparationError, prepare_part_with};

    #[test]
    fn hull_algorithm_panic_is_mapped_to_typed_part_error() {
        let part = ConvexPart::try_new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3],
        )
        .expect("tetrahedron is checked");

        let error = prepare_part_with(7, &part, |_| panic!("injected hull panic"))
            .expect_err("external hull panic must not cross the public boundary");

        assert!(matches!(
            error,
            PhysicsAssetPreparationError::HullAlgorithmPanicked { part_index: 7 }
        ));
    }
}

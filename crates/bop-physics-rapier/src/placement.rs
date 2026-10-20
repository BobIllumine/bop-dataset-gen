use std::{collections::BTreeMap, sync::Arc};

use bop_core::{
    AssetId, DistributionView, InstanceId, ResolvedPhysicalProperties, RigidTransform, RunPlan,
    SpawnVolume,
};
use rapier3d::parry::math::{Pose, Rot3, Vec3};
use thiserror::Error;

use crate::{
    DynamicBodySpec, PlannedInstance, PopulationPlan, PreparedPhysicsAsset,
    placement_sampling::{PlacementField, PlacementSampler},
};

/// Immutable deterministic planner for spawn-contained initial rigid-body poses.
pub struct PlacementPlanner {
    bins: Vec<BinPlacement>,
    placement_attempt_limit_per_object: u32,
    prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
}

impl PlacementPlanner {
    /// Compiles a validated run plan and its already-prepared collision catalog.
    ///
    /// # Errors
    ///
    /// Returns [`PlacementPlanningError::MissingPreparedAsset`] when a positively
    /// selectable configured asset lacks prepared collision geometry.
    pub fn try_new(
        plan: &RunPlan,
        prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
    ) -> Result<Self, PlacementPlanningError> {
        for asset_id in selectable_asset_ids(plan) {
            if !prepared_assets.contains_key(asset_id) {
                return Err(PlacementPlanningError::MissingPreparedAsset {
                    asset_id: asset_id.clone(),
                });
            }
        }
        Ok(Self {
            bins: plan
                .bins()
                .iter()
                .map(|bin| BinPlacement {
                    spawn_volume: bin.population().spawn_volume(),
                    world_from_bin: bin.bin().world_from_bin(),
                })
                .collect(),
            placement_attempt_limit_per_object: plan.run().placement_attempt_limit_per_object(),
            prepared_assets,
        })
    }

    /// Resolves every planned object to a non-overlapping world pose.
    ///
    /// # Errors
    ///
    /// Returns a typed error without publishing a partial plan when any planned
    /// instance is missing geometry, has invalid derived bounds, cannot be
    /// represented by a finite transform, or exhausts its placement attempts.
    pub fn place(
        &self,
        population: &PopulationPlan,
        placement_seed: [u8; 32],
    ) -> Result<PlacementPlan, PlacementPlanningError> {
        let sampler = PlacementSampler::new(placement_seed);
        // Bounds are bin-local, so overlap is only checked within one bin.
        let mut accepted_aabbs = vec![Vec::new(); self.bins.len()];
        let mut placed = Vec::with_capacity(population.instances().len());

        for instance in population.instances() {
            let asset_id = instance.asset_id();
            let prepared_asset =
                Arc::clone(self.prepared_assets.get(asset_id).ok_or_else(|| {
                    PlacementPlanningError::MissingPreparedAsset {
                        asset_id: asset_id.clone(),
                    }
                })?);
            let bin_aabbs = &mut accepted_aabbs[instance.bin_index()];
            let (placed_instance, aabb) =
                self.place_one(instance, prepared_asset, &sampler, bin_aabbs)?;
            bin_aabbs.push(aabb);
            placed.push(placed_instance);
        }
        Ok(PlacementPlan { instances: placed })
    }

    fn place_one(
        &self,
        instance: &PlannedInstance,
        prepared_asset: Arc<PreparedPhysicsAsset>,
        sampler: &PlacementSampler,
        accepted_aabbs: &[PlacementAabb],
    ) -> Result<(PlacedInstance, PlacementAabb), PlacementPlanningError> {
        let BinPlacement {
            spawn_volume,
            world_from_bin,
        } = self.bins[instance.bin_index()];
        for attempt in 0..self.placement_attempt_limit_per_object {
            let orientation = sampler.orientation(instance.instance_id(), attempt).ok_or(
                PlacementPlanningError::NonFiniteTransform {
                    instance_id: instance.instance_id(),
                    attempt,
                },
            )?;
            let orientation_pose = rigid_transform_to_isometry(orientation).ok_or(
                PlacementPlanningError::NonFiniteTransform {
                    instance_id: instance.instance_id(),
                    attempt,
                },
            )?;
            let zero_aabb = PlacementAabb::for_asset(&prepared_asset, &orientation_pose)
                .ok_or_else(|| PlacementPlanningError::InvalidCollisionBounds {
                    asset_id: instance.asset_id().clone(),
                })?;
            let Some((center_min, center_max)) =
                feasible_center_bounds(spawn_volume.min_m(), spawn_volume.max_m(), zero_aabb)
            else {
                continue;
            };
            let center = [
                sample_center(
                    center_min[0],
                    center_max[0],
                    sampler.unit(PlacementField::CenterX, instance.instance_id(), attempt),
                ),
                sample_center(
                    center_min[1],
                    center_max[1],
                    sampler.unit(PlacementField::CenterY, instance.instance_id(), attempt),
                ),
                sample_center(
                    center_min[2],
                    center_max[2],
                    sampler.unit(PlacementField::CenterZ, instance.instance_id(), attempt),
                ),
            ];
            let bin_from_model = RigidTransform::try_new(center, orientation.rotation_xyzw())
                .ok_or(PlacementPlanningError::NonFiniteTransform {
                    instance_id: instance.instance_id(),
                    attempt,
                })?;
            let pose = rigid_transform_to_isometry(bin_from_model).ok_or(
                PlacementPlanningError::NonFiniteTransform {
                    instance_id: instance.instance_id(),
                    attempt,
                },
            )?;
            let aabb = PlacementAabb::for_asset(&prepared_asset, &pose).ok_or_else(|| {
                PlacementPlanningError::InvalidCollisionBounds {
                    asset_id: instance.asset_id().clone(),
                }
            })?;
            if !aabb.contained_by(spawn_volume)
                || accepted_aabbs
                    .iter()
                    .any(|accepted| accepted.conflicts(aabb))
            {
                continue;
            }
            let world_from_model = world_from_bin.compose(bin_from_model).ok_or(
                PlacementPlanningError::NonFiniteTransform {
                    instance_id: instance.instance_id(),
                    attempt,
                },
            )?;
            return Ok((
                PlacedInstance {
                    instance_id: instance.instance_id(),
                    asset_id: instance.asset_id().clone(),
                    bin_index: instance.bin_index(),
                    prepared_asset,
                    world_from_model,
                    physical_properties: instance.physical_properties(),
                },
                aabb,
            ));
        }
        Err(PlacementPlanningError::SpawnSaturated {
            instance_id: instance.instance_id(),
            attempts: self.placement_attempt_limit_per_object,
        })
    }
}

#[derive(Clone, Copy)]
struct BinPlacement {
    spawn_volume: SpawnVolume,
    world_from_bin: RigidTransform,
}

/// A completed, transactional collection of placed instances.
pub struct PlacementPlan {
    instances: Vec<PlacedInstance>,
}

impl PlacementPlan {
    /// Borrows placed instances in the input population's stable order.
    #[must_use]
    pub fn instances(&self) -> &[PlacedInstance] {
        &self.instances
    }

    /// Consumes this plan and returns its placed instances.
    #[must_use]
    pub fn into_instances(self) -> Vec<PlacedInstance> {
        self.instances
    }

    /// Yields borrowed direct handoff specifications without geometry reconstruction.
    #[must_use]
    pub fn dynamic_body_specs(&self) -> impl ExactSizeIterator<Item = DynamicBodySpec<'_>> + '_ {
        self.instances.iter().map(|instance| {
            DynamicBodySpec::new(
                instance.instance_id,
                &instance.asset_id,
                &instance.prepared_asset,
                instance.world_from_model,
                instance.physical_properties,
            )
        })
    }
}

/// One accepted planned instance and its fully resolved pose.
pub struct PlacedInstance {
    instance_id: InstanceId,
    asset_id: AssetId,
    bin_index: usize,
    prepared_asset: Arc<PreparedPhysicsAsset>,
    world_from_model: RigidTransform,
    physical_properties: ResolvedPhysicalProperties,
}

impl PlacedInstance {
    /// Returns the stable foreground identity.
    #[must_use]
    pub const fn instance_id(&self) -> InstanceId {
        self.instance_id
    }
    /// Returns the selected collision asset identifier.
    #[must_use]
    pub const fn asset_id(&self) -> &AssetId {
        &self.asset_id
    }
    /// Returns the index into [`RunPlan::bins`] of the bin this instance was placed in.
    #[must_use]
    pub const fn bin_index(&self) -> usize {
        self.bin_index
    }
    /// Returns the shared prepared collision geometry.
    #[must_use]
    pub fn prepared_asset(&self) -> &PreparedPhysicsAsset {
        &self.prepared_asset
    }
    /// Returns the model-to-world initial pose.
    #[must_use]
    pub const fn world_from_model(&self) -> RigidTransform {
        self.world_from_model
    }
    /// Returns the resolved dynamic-body physical properties.
    #[must_use]
    pub const fn physical_properties(&self) -> ResolvedPhysicalProperties {
        self.physical_properties
    }
}

/// Error returned when placement cannot produce a complete valid plan.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PlacementPlanningError {
    /// A planned or selectable asset lacks prepared collision geometry.
    #[error("placement requires prepared collision geometry for asset {asset_id:?}")]
    MissingPreparedAsset {
        /// The absent logical collision asset identifier.
        asset_id: AssetId,
    },
    /// A prepared collision shape produced non-finite or invalid derived bounds.
    #[error("placement collision bounds are invalid for asset {asset_id:?}")]
    InvalidCollisionBounds {
        /// The logical asset whose derived bounds are unusable.
        asset_id: AssetId,
    },
    /// A sampled or composed candidate transform was not finite.
    #[error("placement transform is non-finite for {instance_id:?} at attempt {attempt}")]
    NonFiniteTransform {
        /// The planned foreground instance whose candidate is invalid.
        instance_id: InstanceId,
        /// The zero-based candidate attempt that could not be represented.
        attempt: u32,
    },
    /// The configured per-object attempt budget was exhausted.
    #[error("placement spawn volume saturated for {instance_id:?} after {attempts} attempts")]
    SpawnSaturated {
        /// The planned foreground instance that could not be placed.
        instance_id: InstanceId,
        /// The configured candidate attempts that were exhausted.
        attempts: u32,
    },
}

#[derive(Clone, Copy)]
struct PlacementAabb {
    min: [f32; 3],
    max: [f32; 3],
}

impl PlacementAabb {
    fn for_asset(asset: &PreparedPhysicsAsset, pose: &Pose) -> Option<Self> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for shape in asset.shapes() {
            let aabb = shape.compute_aabb(pose);
            let mins = [aabb.mins.x, aabb.mins.y, aabb.mins.z];
            let maxs = [aabb.maxs.x, aabb.maxs.y, aabb.maxs.z];
            for axis in 0..3 {
                if !mins[axis].is_finite() || !maxs[axis].is_finite() || mins[axis] > maxs[axis] {
                    return None;
                }
                min[axis] = min[axis].min(mins[axis]);
                max[axis] = max[axis].max(maxs[axis]);
            }
        }
        if min
            .into_iter()
            .zip(max)
            .all(|(min, max)| min.is_finite() && max.is_finite() && min <= max)
        {
            Some(Self { min, max })
        } else {
            None
        }
    }

    fn contained_by(self, spawn: SpawnVolume) -> bool {
        self.contained_by_bounds(spawn.min_m(), spawn.max_m())
    }

    fn contained_by_bounds(self, min: [f32; 3], max: [f32; 3]) -> bool {
        self.min
            .into_iter()
            .zip(min)
            .all(|(aabb, bound)| aabb >= bound)
            && self
                .max
                .into_iter()
                .zip(max)
                .all(|(aabb, bound)| aabb <= bound)
    }

    fn conflicts(self, other: Self) -> bool {
        (0..3).all(|axis| self.min[axis] <= other.max[axis] && other.min[axis] <= self.max[axis])
    }
}

fn feasible_center_bounds(
    spawn_min: [f32; 3],
    spawn_max: [f32; 3],
    aabb: PlacementAabb,
) -> Option<([f32; 3], [f32; 3])> {
    let min = [
        spawn_min[0] - aabb.min[0],
        spawn_min[1] - aabb.min[1],
        spawn_min[2] - aabb.min[2],
    ];
    let max = [
        spawn_max[0] - aabb.max[0],
        spawn_max[1] - aabb.max[1],
        spawn_max[2] - aabb.max[2],
    ];
    min.into_iter()
        .zip(max)
        .all(|(min, max)| min.is_finite() && max.is_finite() && min <= max)
        .then_some((min, max))
}

fn sample_center(min: f32, max: f32, unit: f64) -> f32 {
    if min.to_bits() == max.to_bits() {
        return min;
    }
    #[allow(clippy::cast_possible_truncation)]
    let sampled = (f64::from(min) + (f64::from(max) - f64::from(min)) * unit) as f32;
    if sampled >= max {
        next_down(max)
    } else {
        sampled
    }
}

fn next_down(value: f32) -> f32 {
    if value == 0.0 {
        -f32::from_bits(1)
    } else if value.is_sign_positive() {
        f32::from_bits(value.to_bits() - 1)
    } else {
        f32::from_bits(value.to_bits() + 1)
    }
}

fn rigid_transform_to_isometry(transform: RigidTransform) -> Option<Pose> {
    let [x, y, z] = transform.translation().map(bop_core::Meters::get);
    let [qx, qy, qz, qw] = transform.rotation_xyzw();
    if ![x, y, z, qx, qy, qz, qw].into_iter().all(f32::is_finite) {
        return None;
    }
    Some(Pose::from_parts(
        Vec3::new(x, y, z),
        Rot3::from_xyzw(qx, qy, qz, qw),
    ))
}

fn selectable_asset_ids(plan: &RunPlan) -> Vec<&AssetId> {
    plan.bins()
        .iter()
        .flat_map(|bin| match bin.population().asset_mixture().view() {
            DistributionView::Fixed { value } => vec![value],
            DistributionView::Choice { values } => values
                .iter()
                .filter(|value| value.weight() > 0.0)
                .map(bop_core::WeightedValue::value)
                .collect(),
            DistributionView::Uniform { .. } => {
                unreachable!("validated asset mixtures cannot be uniform")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_1_SQRT_2;

    use bop_assets::ConvexPart;
    use rapier3d::parry::math::{Pose, Rot3, Vec3};

    use crate::PreparedPhysicsAsset;

    use super::{PlacementAabb, feasible_center_bounds, sample_center};

    #[test]
    fn conflict_rejects_touching_but_allows_a_strict_gap() {
        let first = PlacementAabb {
            min: [0.0; 3],
            max: [1.0; 3],
        };
        assert!(first.conflicts(PlacementAabb {
            min: [1.0, 0.0, 0.0],
            max: [2.0, 1.0, 1.0]
        }));
        assert!(first.conflicts(PlacementAabb {
            min: [1.0, 1.0, 1.0],
            max: [2.0, 2.0, 2.0]
        }));
        assert!(first.conflicts(PlacementAabb {
            min: [1.0, 1.0, 0.0],
            max: [2.0, 2.0, 1.0]
        }));
        assert!(!first.conflicts(PlacementAabb {
            min: [1.0 + f32::EPSILON, 0.0, 0.0],
            max: [2.0, 1.0, 1.0]
        }));
    }

    #[test]
    fn rotated_multi_part_aabb_unions_every_shape_and_recomputes_translation() {
        let asset = PreparedPhysicsAsset::try_new(&[
            box_part_at([-2.0, 0.0, 0.0], [0.5, 0.25, 0.25]),
            box_part_at([2.0, 0.0, 0.0], [0.5, 0.25, 0.25]),
        ])
        .expect("two volumetric parts prepare");
        let rotated = Pose::from_parts(
            Vec3::ZERO,
            Rot3::from_xyzw(0.0, 0.0, FRAC_1_SQRT_2, FRAC_1_SQRT_2),
        );
        let rotated_bounds = PlacementAabb::for_asset(&asset, &rotated).expect("finite union");
        assert_eq!(
            rotated_bounds.min.map(f32::to_bits),
            [3_196_059_647, (-2.5_f32).to_bits(), 3_196_059_647]
        );
        assert_eq!(
            rotated_bounds.max.map(f32::to_bits),
            [1_048_575_999, 2.5_f32.to_bits(), 1_048_575_999]
        );

        let translated = Pose::from_parts(
            Vec3::new(3.0, -4.0, 5.0),
            Rot3::from_xyzw(0.0, 0.0, FRAC_1_SQRT_2, FRAC_1_SQRT_2),
        );
        let translated_bounds =
            PlacementAabb::for_asset(&asset, &translated).expect("finite union");
        assert_eq!(
            translated_bounds.min.map(f32::to_bits),
            [2.75_f32.to_bits(), (-6.5_f32).to_bits(), 4.75_f32.to_bits()]
        );
        assert_eq!(
            translated_bounds.max.map(f32::to_bits),
            [3.25_f32.to_bits(), (-1.5_f32).to_bits(), 5.25_f32.to_bits()]
        );
    }

    #[test]
    fn center_bounds_preserve_equal_endpoints_and_reject_inverted_intervals() {
        let aabb = PlacementAabb {
            min: [-1.0, -2.0, -3.0],
            max: [1.0, 2.0, 3.0],
        };
        let (min, max) = feasible_center_bounds([-1.0, -2.0, -3.0], [1.0, 2.0, 3.0], aabb)
            .expect("exactly fitting AABB has a fixed center");
        assert_eq!(min.map(f32::to_bits), [0_u32; 3]);
        assert_eq!(max.map(f32::to_bits), [0_u32; 3]);
        assert_eq!(sample_center(min[0], max[0], 0.999_999).to_bits(), 0);
        assert!(feasible_center_bounds([-0.9, -2.0, -3.0], [0.9, 2.0, 3.0], aabb).is_none());
    }

    #[test]
    fn inclusive_boundary_containment_accepts_exact_faces_and_rejects_one_ulp_escape() {
        let bounds = PlacementAabb {
            min: [-1.0, -2.0, -3.0],
            max: [1.0, 2.0, 3.0],
        };
        assert!(bounds.contained_by_bounds([-1.0, -2.0, -3.0], [1.0, 2.0, 3.0]));
        assert!(
            !PlacementAabb {
                min: [-1.0, -2.0, -3.0],
                max: [f32::from_bits(1.0_f32.to_bits() + 1), 2.0, 3.0]
            }
            .contained_by_bounds([-1.0, -2.0, -3.0], [1.0, 2.0, 3.0])
        );
    }

    fn box_part_at(center: [f32; 3], half_extents: [f32; 3]) -> ConvexPart {
        let [cx, cy, cz] = center;
        let [x, y, z] = half_extents;
        ConvexPart::try_new(
            vec![
                [cx - x, cy - y, cz - z],
                [cx + x, cy - y, cz - z],
                [cx - x, cy + y, cz - z],
                [cx + x, cy + y, cz - z],
                [cx - x, cy - y, cz + z],
                [cx + x, cy - y, cz + z],
                [cx - x, cy + y, cz + z],
                [cx + x, cy + y, cz + z],
            ],
            vec![
                0, 2, 1, 1, 2, 3, 4, 5, 6, 5, 7, 6, 0, 1, 4, 1, 5, 4, 2, 6, 3, 3, 6, 7, 0, 4, 2, 2,
                4, 6, 1, 3, 5, 3, 7, 5,
            ],
        )
        .expect("box is a volumetric checked surface")
    }
}

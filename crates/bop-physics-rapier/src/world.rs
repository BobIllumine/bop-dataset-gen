use std::collections::{BTreeMap, BTreeSet};

use bop_assets::{AnalyticBinCollisionError, analytic_bin_collision};
use bop_core::{
    AssetId, BinConfig, InstanceId, Meters, PhysicsConfig, PhysicsRejectionReason,
    ResolvedPhysicalProperties, RigidTransform,
};
use rapier3d::{
    na::{Isometry3, Quaternion, Translation3, UnitQuaternion, Vector3},
    prelude::{
        BroadPhaseBvh, CCDSolver, Collider, ColliderHandle, ColliderSet, ImpulseJointSet,
        IntegrationParameters, IslandManager, MultibodyJointSet, NarrowPhase, PhysicsPipeline,
        Real, RigidBodyBuilder, RigidBodyHandle, RigidBodySet,
    },
};
use thiserror::Error;

use crate::PreparedPhysicsAsset;

/// Borrowed checked input for one dynamic rigid body.
pub struct DynamicBodySpec<'a> {
    instance_id: InstanceId,
    asset_id: &'a AssetId,
    prepared_asset: &'a PreparedPhysicsAsset,
    world_from_model: RigidTransform,
    physical_properties: ResolvedPhysicalProperties,
}

impl<'a> DynamicBodySpec<'a> {
    /// Creates a dynamic-body specification from a prepared asset and checked metadata.
    #[must_use]
    pub const fn new(
        instance_id: InstanceId,
        asset_id: &'a AssetId,
        prepared_asset: &'a PreparedPhysicsAsset,
        world_from_model: RigidTransform,
        physical_properties: ResolvedPhysicalProperties,
    ) -> Self {
        Self {
            instance_id,
            asset_id,
            prepared_asset,
            world_from_model,
            physical_properties,
        }
    }

    /// Returns the stable ID assigned to this body.
    #[must_use]
    pub const fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    /// Returns the resolved asset ID borrowed by this specification.
    #[must_use]
    pub const fn asset_id(&self) -> &AssetId {
        self.asset_id
    }

    /// Returns the once-prepared collision asset.
    #[must_use]
    pub const fn prepared_asset(&self) -> &'a PreparedPhysicsAsset {
        self.prepared_asset
    }

    /// Returns the initial transform from model coordinates to world coordinates.
    #[must_use]
    pub const fn world_from_model(&self) -> RigidTransform {
        self.world_from_model
    }

    /// Returns the resolved mass and contact properties for this body.
    #[must_use]
    pub const fn physical_properties(&self) -> ResolvedPhysicalProperties {
        self.physical_properties
    }
}

#[allow(dead_code)]
struct DynamicBodyMetadata {
    asset_id: AssetId,
    physical_properties: ResolvedPhysicalProperties,
    body_handle: RigidBodyHandle,
    collider_handles: Vec<ColliderHandle>,
}

struct PreparedDynamicBody {
    instance_id: InstanceId,
    asset_id: AssetId,
    world_from_model: Isometry3<Real>,
    physical_properties: ResolvedPhysicalProperties,
    mass_properties: rapier3d::parry::mass_properties::MassProperties,
    colliders: Vec<Collider>,
}

/// Result of advancing a world through its configured settling window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SettlingOutcome {
    /// Every dynamic body satisfied the configured stability criterion.
    Settled {
        /// Logical steps advanced during this call.
        steps: u32,
        /// Stable checks observed consecutively at completion.
        consecutive_stable_checks: u32,
    },
    /// The configured step budget was exhausted before settling.
    MaximumStepsReached {
        /// Logical steps advanced during this call.
        steps: u32,
        /// Stable checks observed consecutively at completion.
        consecutive_stable_checks: u32,
    },
}

/// Finite aggregate measurements from an accepted final physical state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FinalStateMetrics {
    linear_speed_mps: f32,
    angular_speed_rad_s: f32,
    penetration_m: f32,
}

impl FinalStateMetrics {
    /// Returns the greatest dynamic-body linear speed in metres per second.
    #[must_use]
    pub const fn max_linear_speed_mps(self) -> f32 {
        self.linear_speed_mps
    }

    /// Returns the greatest dynamic-body angular speed in radians per second.
    #[must_use]
    pub const fn max_angular_speed_rad_s(self) -> f32 {
        self.angular_speed_rad_s
    }

    /// Returns the greatest active-contact penetration depth in metres.
    #[must_use]
    pub const fn max_penetration_m(self) -> f32 {
        self.penetration_m
    }
}

/// Reason a settled physical world cannot be published as a scene snapshot.
#[derive(Clone, Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum FinalStateValidationError {
    /// A dynamic body contains a non-finite pose or velocity component.
    #[error("dynamic body {instance_id:?} contains a non-finite final state")]
    NonFiniteState {
        /// Stable identity of the invalid body.
        instance_id: InstanceId,
    },
    /// A body's collision bounds no longer intersect the bin guard prism.
    #[error("dynamic body {instance_id:?} escaped the bin guard prism")]
    Escaped {
        /// Stable identity of the escaped body.
        instance_id: InstanceId,
    },
    /// A final contact penetrates more deeply than configured.
    #[error(
        "contact between {first:?} and {second:?} penetrates {depth_m} m, above limit {limit_m} m"
    )]
    ExcessivePenetration {
        /// First dynamic instance, or `None` for the fixed bin.
        first: Option<InstanceId>,
        /// Second dynamic instance, or `None` for the fixed bin.
        second: Option<InstanceId>,
        /// Observed penetration depth in metres.
        depth_m: f32,
        /// Configured inclusive limit in metres.
        limit_m: f32,
    },
    /// Stored body/collider metadata is internally inconsistent.
    #[error("invalid final physics state: {message}")]
    InvalidFinalState {
        /// Stable diagnostic without backend handles in the public type.
        message: String,
    },
}

impl FinalStateValidationError {
    /// Maps the detailed validation error to its deterministic report category.
    #[must_use]
    pub const fn rejection_reason(&self) -> PhysicsRejectionReason {
        match self {
            Self::NonFiniteState { .. } => PhysicsRejectionReason::NonFiniteState,
            Self::Escaped { .. } => PhysicsRejectionReason::Escape,
            Self::ExcessivePenetration { .. } => PhysicsRejectionReason::ExcessivePenetration,
            Self::InvalidFinalState { .. } => PhysicsRejectionReason::InvalidFinalState,
        }
    }
}

impl SettlingOutcome {
    /// Returns the logical step count advanced during this call.
    #[must_use]
    pub const fn steps(self) -> u32 {
        match self {
            Self::Settled { steps, .. } | Self::MaximumStepsReached { steps, .. } => steps,
        }
    }

    /// Returns the final consecutive stable-check count for this call.
    #[must_use]
    pub const fn consecutive_stable_checks(self) -> u32 {
        match self {
            Self::Settled {
                consecutive_stable_checks,
                ..
            }
            | Self::MaximumStepsReached {
                consecutive_stable_checks,
                ..
            } => consecutive_stable_checks,
        }
    }

    /// Returns whether the world satisfied the configured settling criterion.
    #[must_use]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::Settled { .. })
    }
}

#[derive(Default)]
struct StabilityState {
    consecutive_stable_checks: u32,
}

impl StabilityState {
    fn observe(&mut self, step: u32, interval: u32, stable: bool) -> bool {
        if !step.is_multiple_of(interval) {
            return false;
        }
        if stable {
            self.consecutive_stable_checks += 1;
        } else {
            self.consecutive_stable_checks = 0;
        }
        true
    }

    const fn consecutive_stable_checks(&self) -> u32 {
        self.consecutive_stable_checks
    }

    const fn can_settle(&self, step: u32, min_steps: u32, required_stable_checks: u32) -> bool {
        step >= min_steps && self.consecutive_stable_checks >= required_stable_checks
    }
}

/// A serial Rapier world containing the fixed analytic bin.
#[allow(dead_code)]
pub struct PhysicsWorld {
    physics_pipeline: PhysicsPipeline,
    integration_parameters: IntegrationParameters,
    island_manager: IslandManager,
    broad_phase: BroadPhaseBvh,
    narrow_phase: NarrowPhase,
    rigid_bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd_solver: CCDSolver,
    gravity: Vector3<Real>,
    bin_bodies: Vec<RigidBodyHandle>,
    bin_collider_count: usize,
    dynamic_bodies: BTreeMap<InstanceId, DynamicBodyMetadata>,
}

/// Error returned when a physics world cannot be constructed, populated, or queried.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PhysicsWorldError {
    /// Deriving finite analytic bin collision geometry failed.
    #[error(transparent)]
    AnalyticBin(#[from] AnalyticBinCollisionError),
    /// A Rapier pose could not be represented as a finite core transform.
    #[error("Rapier pose cannot be represented as a finite core transform")]
    NonFiniteTransform,
    /// A submitted instance ID is duplicated in the batch or already exists in the world.
    #[error("duplicate dynamic-body instance ID {instance_id:?}")]
    DuplicateInstanceId {
        /// The duplicate stable instance ID.
        instance_id: InstanceId,
    },
    /// A new ID would violate global Rapier insertion order across batches.
    #[error(
        "dynamic-body instance ID {instance_id:?} is not greater than previous maximum {previous_maximum:?}"
    )]
    InstanceIdNotAfterPreviousMaximum {
        /// The rejected stable instance ID.
        instance_id: InstanceId,
        /// The greatest stable instance ID already stored in the world.
        previous_maximum: InstanceId,
    },
    /// Stored metadata refers to a Rapier rigid body that is no longer present.
    #[error("stored dynamic-body metadata for {instance_id:?} has no Rapier rigid body")]
    MissingRigidBody {
        /// The stable instance ID whose Rapier body is missing.
        instance_id: InstanceId,
    },
    /// The checked solver iteration count cannot be represented by Rapier.
    #[error("solver iteration count {solver_iterations} cannot be represented by Rapier")]
    SolverIterationsCannotFit {
        /// The checked configured solver iteration count.
        solver_iterations: u32,
    },
}

impl PhysicsWorld {
    /// Creates a serial Rapier world containing exactly one fixed analytic bin.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsWorldError::AnalyticBin`] when the checked bin values
    /// overflow while deriving its analytic collision geometry.
    pub fn try_new(bin: &BinConfig) -> Result<Self, PhysicsWorldError> {
        Self::try_with_bins([bin])
    }

    /// Creates a serial Rapier world containing one fixed analytic body per bin.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsWorldError::AnalyticBin`] when the checked bin values
    /// overflow while deriving their analytic collision geometry.
    pub fn try_with_bins<'a>(
        bins: impl IntoIterator<Item = &'a BinConfig>,
    ) -> Result<Self, PhysicsWorldError> {
        let gravity = Vector3::new(0.0, 0.0, -9.81);
        let mut rigid_bodies = RigidBodySet::new();
        let mut colliders = ColliderSet::new();
        let mut bin_bodies = Vec::new();
        let mut bin_collider_count = 0;
        for bin in bins {
            let bin_body = rigid_bodies.insert(
                RigidBodyBuilder::fixed()
                    .pose(rigid_transform_to_isometry(bin.world_from_bin())?.into())
                    .build(),
            );
            let physical_properties = bin.physical_properties();
            let cuboids = analytic_bin_collision(bin)?.cuboids();
            for cuboid in cuboids {
                let [x, y, z] = cuboid.half_extents_m().map(bop_core::PositiveMeters::get);
                let collider = rapier3d::prelude::ColliderBuilder::cuboid(x, y, z)
                    .position(rigid_transform_to_isometry(cuboid.bin_from_cuboid())?.into())
                    .friction(physical_properties.friction())
                    .restitution(physical_properties.restitution())
                    .build();
                colliders.insert_with_parent(collider, bin_body, &mut rigid_bodies);
            }
            bin_bodies.push(bin_body);
            bin_collider_count += cuboids.len();
        }

        Ok(Self {
            physics_pipeline: PhysicsPipeline::new(),
            integration_parameters: IntegrationParameters::default(),
            island_manager: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            rigid_bodies,
            colliders,
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            gravity,
            bin_bodies,
            bin_collider_count,
            dynamic_bodies: BTreeMap::new(),
        })
    }

    /// Advances this world through its configured fixed logical steps.
    ///
    /// Each logical step is subdivided into the configured number of serial
    /// Rapier pipeline calls, followed by its scheduled stability check.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsWorldError::SolverIterationsCannotFit`] when the
    /// checked solver iteration count cannot be represented by Rapier, or
    /// [`PhysicsWorldError::MissingRigidBody`] if internal dynamic-body
    /// metadata no longer resolves to its Rapier body during a stability check.
    pub fn settle(&mut self, config: PhysicsConfig) -> Result<SettlingOutcome, PhysicsWorldError> {
        self.configure_integration_parameters(
            config.fixed_timestep_s(),
            config.substeps(),
            config.solver_iterations(),
        )?;

        let mut stability = StabilityState::default();
        for step in 1..=config.max_steps() {
            for _ in 0..config.substeps() {
                self.physics_pipeline.step(
                    self.gravity.into(),
                    &self.integration_parameters,
                    &mut self.island_manager,
                    &mut self.broad_phase,
                    &mut self.narrow_phase,
                    &mut self.rigid_bodies,
                    &mut self.colliders,
                    &mut self.impulse_joints,
                    &mut self.multibody_joints,
                    &mut self.ccd_solver,
                    &(),
                    &(),
                );
            }

            if step.is_multiple_of(config.stability_check_interval()) {
                let stable = self.dynamic_bodies_are_stable(
                    config.linear_sleep_threshold_mps(),
                    config.angular_sleep_threshold_rad_s(),
                )?;
                let checked = stability.observe(step, config.stability_check_interval(), stable);
                debug_assert!(checked);
                if stability.can_settle(step, config.min_steps(), config.required_stable_checks()) {
                    return Ok(SettlingOutcome::Settled {
                        steps: step,
                        consecutive_stable_checks: stability.consecutive_stable_checks(),
                    });
                }
            }
        }

        Ok(SettlingOutcome::MaximumStepsReached {
            steps: config.max_steps(),
            consecutive_stable_checks: stability.consecutive_stable_checks(),
        })
    }

    /// Validates all dynamic bodies and returns aggregate final-state measurements.
    ///
    /// Escape is defined geometrically: each body's union collider AABB must
    /// intersect the open bin's outer guard prism, expanded by the configured
    /// escape margin. Final contact penetration is accepted inclusively at the
    /// configured limit.
    ///
    /// # Errors
    ///
    /// Returns a deterministic typed rejection for non-finite state, excessive
    /// penetration, escape, or inconsistent internal metadata.
    pub fn validate_final_state(
        &self,
        bin: &BinConfig,
        config: PhysicsConfig,
    ) -> Result<FinalStateMetrics, FinalStateValidationError> {
        self.validate_final_state_in_bins(&[bin], |_| 0, config)
    }

    /// Like [`Self::validate_final_state`], but each body must stay in its
    /// own bin: `bin_of` maps an instance to its index in `bins`.
    ///
    /// # Errors
    ///
    /// Returns the same rejections; an unknown bin index is an invalid state.
    pub fn validate_final_state_in_bins(
        &self,
        bins: &[&BinConfig],
        bin_of: impl Fn(InstanceId) -> usize,
        config: PhysicsConfig,
    ) -> Result<FinalStateMetrics, FinalStateValidationError> {
        let guards = bins
            .iter()
            .map(|bin| bin_guard(bin, config.escape_margin_m()))
            .collect::<Result<Vec<_>, _>>()?;
        let (max_linear_squared, max_angular_squared, first_escape) =
            self.validate_dynamic_bodies(&guards, bin_of)?;
        let deepest = self.deepest_contact();
        if penetration_exceeds_limit(deepest.0, config.maximum_penetration_m()) {
            return Err(FinalStateValidationError::ExcessivePenetration {
                first: deepest.1,
                second: deepest.2,
                depth_m: deepest.0,
                limit_m: config.maximum_penetration_m(),
            });
        }
        if let Some(instance_id) = first_escape {
            return Err(FinalStateValidationError::Escaped { instance_id });
        }

        Ok(FinalStateMetrics {
            linear_speed_mps: canonicalize_zero_f64(max_linear_squared.sqrt()),
            angular_speed_rad_s: canonicalize_zero_f64(max_angular_squared.sqrt()),
            penetration_m: deepest.0,
        })
    }

    fn validate_dynamic_bodies(
        &self,
        guards: &[BinGuard],
        bin_of: impl Fn(InstanceId) -> usize,
    ) -> Result<(f64, f64, Option<InstanceId>), FinalStateValidationError> {
        let mut max_linear_squared = 0.0_f64;
        let mut max_angular_squared = 0.0_f64;
        let mut first_escape = None;
        for (&instance_id, metadata) in &self.dynamic_bodies {
            let body = self.rigid_bodies.get(metadata.body_handle).ok_or_else(|| {
                invalid_final_state(format!(
                    "body metadata for {instance_id:?} has no rigid body"
                ))
            })?;
            let position = body.position();
            let rotation = position.rotation;
            let linear = body.linvel();
            let angular = body.angvel();
            if ![
                position.translation.x,
                position.translation.y,
                position.translation.z,
                rotation.x,
                rotation.y,
                rotation.z,
                rotation.w,
                linear.x,
                linear.y,
                linear.z,
                angular.x,
                angular.y,
                angular.z,
            ]
            .into_iter()
            .all(f32::is_finite)
            {
                return Err(FinalStateValidationError::NonFiniteState { instance_id });
            }
            max_linear_squared = max_linear_squared.max(norm_squared(linear.x, linear.y, linear.z));
            max_angular_squared =
                max_angular_squared.max(norm_squared(angular.x, angular.y, angular.z));

            let BinGuard {
                world_to_bin,
                min: guard_min,
                max: guard_max,
            } = *guards.get(bin_of(instance_id)).ok_or_else(|| {
                invalid_final_state(format!("{instance_id:?} belongs to no configured bin"))
            })?;
            let mut body_min = [f32::INFINITY; 3];
            let mut body_max = [f32::NEG_INFINITY; 3];
            if metadata.collider_handles.is_empty() {
                return Err(invalid_final_state(format!(
                    "body metadata for {instance_id:?} has no colliders"
                )));
            }
            for &handle in &metadata.collider_handles {
                let collider = self.colliders.get(handle).ok_or_else(|| {
                    invalid_final_state(format!(
                        "body metadata for {instance_id:?} has a missing collider"
                    ))
                })?;
                let aabb = collider.compute_aabb();
                for x in [aabb.mins.x, aabb.maxs.x] {
                    for y in [aabb.mins.y, aabb.maxs.y] {
                        for z in [aabb.mins.z, aabb.maxs.z] {
                            let point = [x, y, z].map(Meters::new);
                            let Some([x, y, z]) = point[0]
                                .zip(point[1])
                                .zip(point[2])
                                .and_then(|((x, y), z)| world_to_bin.transform_point([x, y, z]))
                            else {
                                return Err(FinalStateValidationError::NonFiniteState {
                                    instance_id,
                                });
                            };
                            for (axis, value) in [x.get(), y.get(), z.get()].into_iter().enumerate()
                            {
                                body_min[axis] = body_min[axis].min(value);
                                body_max[axis] = body_max[axis].max(value);
                            }
                        }
                    }
                }
            }
            if (0..3)
                .any(|axis| body_max[axis] < guard_min[axis] || body_min[axis] > guard_max[axis])
            {
                first_escape.get_or_insert(instance_id);
            }
        }
        Ok((max_linear_squared, max_angular_squared, first_escape))
    }

    fn deepest_contact(&self) -> (f32, Option<InstanceId>, Option<InstanceId>) {
        let mut deepest = (0.0_f32, None, None);
        for pair in self.narrow_phase.contact_pairs() {
            let (first, second) = ordered_contact_instances(
                self.instance_for_collider(pair.collider1),
                self.instance_for_collider(pair.collider2),
            );
            if first.is_none() && second.is_none() {
                continue;
            }
            for contact in pair
                .manifolds
                .iter()
                .flat_map(|manifold| manifold.points.iter())
            {
                let depth = (-contact.dist).max(0.0);
                if depth > deepest.0 {
                    deepest = (depth, first, second);
                }
            }
        }
        deepest
    }

    fn instance_for_collider(&self, handle: ColliderHandle) -> Option<InstanceId> {
        self.dynamic_bodies
            .iter()
            .find_map(|(&instance_id, metadata)| {
                metadata
                    .collider_handles
                    .contains(&handle)
                    .then_some(instance_id)
            })
    }

    fn configure_integration_parameters(
        &mut self,
        fixed_timestep_s: f32,
        substeps: u32,
        solver_iterations: u32,
    ) -> Result<(), PhysicsWorldError> {
        // Rapier's `Real` is f32 while the checked public configuration keeps
        // its nonzero substep count as u32. This conversion is required at the
        // API boundary and occurs once per `settle` call, outside its hot loop.
        #[allow(clippy::cast_precision_loss)]
        let substeps = substeps as Real;
        self.integration_parameters.dt = fixed_timestep_s / substeps;
        self.integration_parameters.num_solver_iterations = usize::try_from(solver_iterations)
            .map_err(|_| PhysicsWorldError::SolverIterationsCannotFit { solver_iterations })?;
        Ok(())
    }

    fn dynamic_bodies_are_stable(
        &self,
        linear_threshold: f32,
        angular_threshold: f32,
    ) -> Result<bool, PhysicsWorldError> {
        let linear_threshold_squared = f64::from(linear_threshold).powi(2);
        let angular_threshold_squared = f64::from(angular_threshold).powi(2);

        for (&instance_id, metadata) in &self.dynamic_bodies {
            let body = self
                .rigid_bodies
                .get(metadata.body_handle)
                .ok_or(PhysicsWorldError::MissingRigidBody { instance_id })?;
            let linear = body.linvel();
            let angular = body.angvel();
            let components = [
                linear.x, linear.y, linear.z, angular.x, angular.y, angular.z,
            ];
            if !components.into_iter().all(f32::is_finite) {
                return Ok(false);
            }
            let linear_squared = f64::from(linear.x).powi(2)
                + f64::from(linear.y).powi(2)
                + f64::from(linear.z).powi(2);
            let angular_squared = f64::from(angular.x).powi(2)
                + f64::from(angular.y).powi(2)
                + f64::from(angular.z).powi(2);
            if linear_squared > linear_threshold_squared
                || angular_squared > angular_threshold_squared
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Returns the world's constant Z-up gravitational acceleration in m/s².
    #[must_use]
    pub fn gravity_m_s2(&self) -> [f32; 3] {
        [self.gravity.x, self.gravity.y, self.gravity.z]
    }

    /// Returns the number of analytic colliders attached to the fixed bin bodies.
    #[must_use]
    pub const fn bin_collider_count(&self) -> usize {
        self.bin_collider_count
    }

    /// Returns the number of inserted dynamic rigid bodies.
    #[must_use]
    pub fn dynamic_body_count(&self) -> usize {
        self.dynamic_bodies.len()
    }

    /// Iterates inserted body IDs in ascending stable-ID order.
    #[must_use]
    pub fn instance_ids(&self) -> impl ExactSizeIterator<Item = InstanceId> + '_ {
        self.dynamic_bodies.keys().copied()
    }

    /// Returns an inserted body's current world pose, if it exists.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsWorldError::NonFiniteTransform`] if the stored Rapier
    /// pose cannot be represented by a checked core transform.
    pub fn body_pose(
        &self,
        instance_id: InstanceId,
    ) -> Result<Option<RigidTransform>, PhysicsWorldError> {
        let Some(metadata) = self.dynamic_bodies.get(&instance_id) else {
            return Ok(None);
        };
        let Some(body) = self.rigid_bodies.get(metadata.body_handle) else {
            return Err(PhysicsWorldError::MissingRigidBody { instance_id });
        };
        rigid_transform_from_isometry((*body.position()).into()).map(Some)
    }

    /// Returns the number of colliders attached to an inserted dynamic body.
    #[must_use]
    pub fn collider_count(&self, instance_id: InstanceId) -> Option<usize> {
        self.dynamic_bodies
            .get(&instance_id)
            .map(|metadata| metadata.collider_handles.len())
    }

    /// Validates and inserts a complete dynamic-body batch in stable-ID order.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the world when an ID is duplicate or
    /// violates global insertion order, or when a checked transform cannot be
    /// represented by Rapier.
    pub fn insert_dynamic_bodies(
        &mut self,
        bodies: &[DynamicBodySpec<'_>],
    ) -> Result<(), PhysicsWorldError> {
        let mut submitted_ids = BTreeSet::new();
        let mut prepared = Vec::with_capacity(bodies.len());

        for body in bodies {
            let instance_id = body.instance_id();
            if !submitted_ids.insert(instance_id) || self.dynamic_bodies.contains_key(&instance_id)
            {
                return Err(PhysicsWorldError::DuplicateInstanceId { instance_id });
            }
        }

        if let Some((&previous_maximum, _)) = self.dynamic_bodies.last_key_value()
            && let Some(instance_id) = bodies
                .iter()
                .map(DynamicBodySpec::instance_id)
                .find(|&instance_id| instance_id <= previous_maximum)
        {
            return Err(PhysicsWorldError::InstanceIdNotAfterPreviousMaximum {
                instance_id,
                previous_maximum,
            });
        }

        for body in bodies {
            let instance_id = body.instance_id();
            let colliders = body
                .prepared_asset()
                .shapes()
                .iter()
                .map(|shape| {
                    rapier3d::prelude::ColliderBuilder::new(shape.clone())
                        .density(0.0)
                        .friction(body.physical_properties().friction())
                        .restitution(body.physical_properties().restitution())
                        .build()
                })
                .collect();

            prepared.push(PreparedDynamicBody {
                instance_id,
                asset_id: body.asset_id().clone(),
                world_from_model: rigid_transform_to_isometry(body.world_from_model())?,
                physical_properties: body.physical_properties(),
                mass_properties: body
                    .prepared_asset()
                    .mass_properties(body.physical_properties().mass_kg()),
                colliders,
            });
        }

        prepared.sort_unstable_by_key(|body| body.instance_id);
        for body in prepared {
            let body_handle = self.rigid_bodies.insert(
                RigidBodyBuilder::dynamic()
                    .pose(body.world_from_model.into())
                    .additional_mass_properties(body.mass_properties)
                    .build(),
            );
            let collider_handles = body
                .colliders
                .into_iter()
                .map(|collider| {
                    self.colliders
                        .insert_with_parent(collider, body_handle, &mut self.rigid_bodies)
                })
                .collect();
            self.rigid_bodies[body_handle]
                .recompute_mass_properties_from_colliders(&self.colliders);
            self.dynamic_bodies.insert(
                body.instance_id,
                DynamicBodyMetadata {
                    asset_id: body.asset_id,
                    physical_properties: body.physical_properties,
                    body_handle,
                    collider_handles,
                },
            );
        }

        Ok(())
    }
}

/// A bin's outer guard prism, expanded by the escape margin, in bin coordinates.
#[derive(Clone, Copy)]
struct BinGuard {
    world_to_bin: RigidTransform,
    min: [f32; 3],
    max: [f32; 3],
}

fn bin_guard(bin: &BinConfig, margin: f32) -> Result<BinGuard, FinalStateValidationError> {
    let world_to_bin = bin.world_from_bin().inverse().ok_or_else(|| {
        FinalStateValidationError::InvalidFinalState {
            message: "bin transform has no finite inverse".to_owned(),
        }
    })?;
    let dimensions = bin.inner_dimensions_m().map(bop_core::PositiveMeters::get);
    let wall = bin.wall_thickness_m().get();
    Ok(BinGuard {
        world_to_bin,
        min: [
            -dimensions[0] / 2.0 - wall - margin,
            -dimensions[1] / 2.0 - wall - margin,
            -bin.floor_thickness_m().get() - margin,
        ],
        max: [
            dimensions[0] / 2.0 + wall + margin,
            dimensions[1] / 2.0 + wall + margin,
            dimensions[2] + margin,
        ],
    })
}

fn invalid_final_state(message: String) -> FinalStateValidationError {
    FinalStateValidationError::InvalidFinalState { message }
}

fn norm_squared(x: f32, y: f32, z: f32) -> f64 {
    f64::from(x).powi(2) + f64::from(y).powi(2) + f64::from(z).powi(2)
}

fn penetration_exceeds_limit(depth_m: f32, limit_m: f32) -> bool {
    depth_m > limit_m
}

fn ordered_contact_instances(
    first: Option<InstanceId>,
    second: Option<InstanceId>,
) -> (Option<InstanceId>, Option<InstanceId>) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

#[allow(clippy::cast_possible_truncation)]
fn canonicalize_zero_f64(value: f64) -> f32 {
    let value = value as f32;
    if value == 0.0 { 0.0 } else { value }
}

fn rigid_transform_to_isometry(
    transform: RigidTransform,
) -> Result<Isometry3<Real>, PhysicsWorldError> {
    let [x, y, z] = transform.translation().map(bop_core::Meters::get);
    let [qx, qy, qz, qw] = transform.rotation_xyzw();
    if ![x, y, z, qx, qy, qz, qw].into_iter().all(f32::is_finite) {
        return Err(PhysicsWorldError::NonFiniteTransform);
    }

    Ok(Isometry3::from_parts(
        Translation3::new(x, y, z),
        UnitQuaternion::from_quaternion(Quaternion::new(qw, qx, qy, qz)),
    ))
}

#[allow(dead_code)]
fn rigid_transform_from_isometry(
    isometry: Isometry3<Real>,
) -> Result<RigidTransform, PhysicsWorldError> {
    let translation = isometry.translation.vector;
    let quaternion = isometry.rotation.quaternion();
    RigidTransform::try_new(
        [translation.x, translation.y, translation.z],
        [quaternion.i, quaternion.j, quaternion.k, quaternion.w],
    )
    .ok_or(PhysicsWorldError::NonFiniteTransform)
}

#[cfg(test)]
mod tests {
    use bop_assets::{ConvexPart, analytic_bin_collision};
    use bop_core::{AssetId, InstanceId, ResolvedPhysicalProperties, RunConfig};
    use rapier3d::na::{Isometry3, Vector3};

    use super::{
        DynamicBodySpec, FinalStateValidationError, PhysicsWorld, PhysicsWorldError,
        penetration_exceeds_limit, rigid_transform_from_isometry, rigid_transform_to_isometry,
    };
    use crate::PreparedPhysicsAsset;

    use super::StabilityState;

    #[test]
    fn stability_state_checks_only_divisible_one_based_steps() {
        let mut state = StabilityState::default();

        assert!(!state.observe(1, 2, true));
        assert_eq!(state.consecutive_stable_checks(), 0);
        assert!(state.observe(2, 2, true));
        assert_eq!(state.consecutive_stable_checks(), 1);
        assert!(!state.observe(3, 2, false));
        assert_eq!(state.consecutive_stable_checks(), 1);
        assert!(state.observe(4, 2, false));
        assert_eq!(state.consecutive_stable_checks(), 0);
    }

    #[test]
    fn stable_checks_before_minimum_steps_accumulate_without_settling_early() {
        let mut state = StabilityState::default();

        assert!(state.observe(1, 1, true));
        assert!(!state.can_settle(1, 3, 2));
        assert!(state.observe(2, 1, true));
        assert!(!state.can_settle(2, 3, 2));
        assert!(state.can_settle(3, 3, 2));
    }

    #[test]
    fn exact_velocity_threshold_equality_is_stable() {
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let mut world = world_with_one_dynamic_body(instance_id);
        let body_handle = world.dynamic_bodies[&instance_id].body_handle;
        world.rigid_bodies[body_handle].set_linvel(Vector3::new(3.0, 4.0, 0.0).into(), true);

        assert!(
            world
                .dynamic_bodies_are_stable(5.0, 0.0)
                .expect("stored body exists")
        );
    }

    #[test]
    fn one_representable_velocity_value_above_threshold_is_unstable() {
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let mut world = world_with_one_dynamic_body(instance_id);
        let body_handle = world.dynamic_bodies[&instance_id].body_handle;
        let above = f32::from_bits(5.0_f32.to_bits() + 1);
        world.rigid_bodies[body_handle].set_linvel(Vector3::new(above, 0.0, 0.0).into(), true);

        assert!(
            !world
                .dynamic_bodies_are_stable(5.0, 0.0)
                .expect("stored body exists")
        );
    }

    #[test]
    fn nonfinite_velocity_is_unstable() {
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let mut world = world_with_one_dynamic_body(instance_id);
        let body_handle = world.dynamic_bodies[&instance_id].body_handle;
        world.rigid_bodies[body_handle].set_linvel(Vector3::new(f32::NAN, 0.0, 0.0).into(), true);

        assert!(
            !world
                .dynamic_bodies_are_stable(100.0, 100.0)
                .expect("stored body exists")
        );
    }

    #[test]
    fn one_unstable_body_prevents_an_otherwise_stable_population_from_settling() {
        let first = InstanceId::new(1).expect("nonzero instance ID");
        let second = InstanceId::new(2).expect("nonzero instance ID");
        let mut world = world_with_dynamic_bodies(&[first, second]);
        let second_handle = world.dynamic_bodies[&second].body_handle;
        world.rigid_bodies[second_handle].set_angvel(Vector3::new(0.0, 0.0, 2.0).into(), true);

        assert!(
            !world
                .dynamic_bodies_are_stable(0.0, 1.0)
                .expect("stored bodies exist")
        );
    }

    #[test]
    fn penetration_limit_is_inclusive_at_equality() {
        let limit = 0.002_f32;

        assert!(!penetration_exceeds_limit(limit, limit));
        assert!(penetration_exceeds_limit(
            f32::from_bits(limit.to_bits() + 1),
            limit
        ));
    }

    #[test]
    fn final_state_rejects_nonfinite_body_state_before_geometry() {
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let mut world = world_with_one_dynamic_body(instance_id);
        let body_handle = world.dynamic_bodies[&instance_id].body_handle;
        world.rigid_bodies[body_handle].set_linvel(Vector3::new(f32::NAN, 0.0, 0.0).into(), true);
        let plan = test_plan([0.0; 3], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1);

        assert!(matches!(
            world.validate_final_state(plan.bin(), *plan.physics()),
            Err(FinalStateValidationError::NonFiniteState { instance_id: id }) if id == instance_id
        ));
    }

    #[test]
    fn final_state_reports_missing_stored_body_as_invalid_state() {
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let mut world = world_with_one_dynamic_body(instance_id);
        let body_handle = world.dynamic_bodies[&instance_id].body_handle;
        world.rigid_bodies.remove(
            body_handle,
            &mut world.island_manager,
            &mut world.colliders,
            &mut world.impulse_joints,
            &mut world.multibody_joints,
            true,
        );
        let plan = test_plan([0.0; 3], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1);

        assert!(matches!(
            world.validate_final_state(plan.bin(), *plan.physics()),
            Err(FinalStateValidationError::InvalidFinalState { .. })
        ));
    }

    #[test]
    fn final_state_metrics_take_component_complete_multi_body_maxima() {
        let first = InstanceId::new(1).expect("nonzero instance ID");
        let second = InstanceId::new(2).expect("nonzero instance ID");
        let mut world = world_with_dynamic_bodies(&[first, second]);
        let first_handle = world.dynamic_bodies[&first].body_handle;
        let second_handle = world.dynamic_bodies[&second].body_handle;
        world.rigid_bodies[first_handle].set_linvel(Vector3::new(3.0, 4.0, 0.0).into(), true);
        world.rigid_bodies[second_handle].set_angvel(Vector3::new(0.0, 0.0, 12.0).into(), true);
        let plan = test_plan([0.0; 3], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1);

        let metrics = world
            .validate_final_state(plan.bin(), *plan.physics())
            .expect("finite bodies inside the bin are measurable");

        assert_eq!(metrics.max_linear_speed_mps().to_bits(), 5.0_f32.to_bits());
        assert_eq!(
            metrics.max_angular_speed_rad_s().to_bits(),
            12.0_f32.to_bits()
        );
    }

    #[test]
    fn configured_solver_iterations_and_substep_duration_reach_rapier() {
        let mut world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");

        world
            .configure_integration_parameters(0.1, 4, 3)
            .expect("checked solver iteration count fits");

        assert_eq!(
            world.integration_parameters.dt.to_bits(),
            0.025_f32.to_bits()
        );
        assert_eq!(world.integration_parameters.num_solver_iterations, 3);
    }

    fn bin(
        translation: [f32; 3],
        rotation_xyzw: [f32; 4],
        friction: f32,
        restitution: f32,
    ) -> bop_core::BinConfig {
        test_plan(translation, rotation_xyzw, friction, restitution)
            .bin()
            .clone()
    }

    fn test_plan(
        translation: [f32; 3],
        rotation_xyzw: [f32; 4],
        friction: f32,
        restitution: f32,
    ) -> bop_core::RunPlan {
        let [tx, ty, tz] = translation;
        let [rx, ry, rz, rw] = rotation_xyzw;
        let input = format!(
            r#"[run]
master_seed = 1
valid_scene_count = 1
simulation_workers = 1
renderer_workers = 1
queue_capacity = 1
output_path = "output"
cache_path = "cache"

[bin]
inner_dimensions_m = [1.0, 0.8, 0.5]
floor_thickness_m = 0.02
wall_thickness_m = 0.03
world_from_bin = {{ translation = [{tx:e}, {ty:e}, {tz:e}], rotation_xyzw = [{rx:e}, {ry:e}, {rz:e}, {rw:e}] }}
material_label = "steel"
physical_properties = {{ friction = {friction:e}, restitution = {restitution:e} }}

[[assets]]
id = "part"
source_path = "part.glb"
source_unit_scale_m = 1.0
model_transform = {{ translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
mixture_weight = 1.0
physical_properties = {{ mass_kg = {{ kind = "fixed", value = 0.5 }}, friction = {{ kind = "fixed", value = 0.5 }}, restitution = {{ kind = "fixed", value = 0.1 }} }}
vhacd = {{ resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }}

[population]
object_count = {{ kind = "fixed", value = 1 }}
asset_mixture = {{ kind = "fixed", value = "part" }}
spawn_volume = {{ min_m = [-0.25, -0.2, 0.0], max_m = [0.25, 0.2, 0.25] }}

[physics]
substeps = 1
solver_iterations = 1

[[cameras]]
id = 1
image_size = {{ width = 16, height = 16 }}
near_m = 0.01
far_m = 10.0
projection = {{ vertical_fov_deg = 55.0 }}
pose = {{ fixed_world_from_camera = {{ translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }} }}

[appearance]
background_srgb = [0.0, 0.0, 0.0, 1.0]
ambient = {{ color = [1.0, 1.0, 1.0], intensity = 0.0 }}
directional_lights = []
point_lights = []
material_defaults = {{ base_color_srgb = [0.5, 0.5, 0.5, 1.0], metallic = {{ kind = "fixed", value = 0.0 }}, roughness = {{ kind = "fixed", value = 0.5 }} }}

[renderers.wgpu]
device_preference = "automatic"
adapter_preference = "high_performance"
shadow_map_size = 16
rgb_fxaa = false

[renderers.blenderproc]
executable_path = "blenderproc"
worker_path = "worker.py"
device_selection = "CPU"
handshake_timeout_s = 1.0
request_timeout_s = 1.0
stderr_tail_capacity = 64

[output]
rgb = true
depth = false
visible_instances = false
"#,
        );
        RunConfig::from_toml(&input)
            .expect("test configuration parses")
            .into_plan()
    }

    fn world_with_one_dynamic_body(instance_id: InstanceId) -> PhysicsWorld {
        world_with_dynamic_bodies(&[instance_id])
    }

    fn world_with_dynamic_bodies(instance_ids: &[InstanceId]) -> PhysicsWorld {
        let asset_id = AssetId::new("part").expect("nonempty asset ID");
        let properties = ResolvedPhysicalProperties::try_new(2.5, 0.73, 0.27)
            .expect("valid physical properties");
        let parts = vec![
            ConvexPart::try_new(
                vec![
                    [0.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                ],
                vec![0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3],
            )
            .expect("tetrahedron is checked"),
        ];
        let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
        let mut world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");
        let bodies = instance_ids
            .iter()
            .copied()
            .map(|instance_id| {
                DynamicBodySpec::new(
                    instance_id,
                    &asset_id,
                    &asset,
                    bop_core::RigidTransform::identity(),
                    properties,
                )
            })
            .collect::<Vec<_>>();
        world
            .insert_dynamic_bodies(&bodies)
            .expect("checked body inserts");
        world
    }

    #[test]
    fn transform_bridge_round_trips_translation_and_xyzw_quaternion_order() {
        let transform = bop_core::RigidTransform::try_new(
            [1.25, -2.5, 3.75],
            [0.0, 0.0, 0.5_f32.sqrt(), 0.5_f32.sqrt()],
        )
        .expect("finite nonzero transform");

        let isometry = rigid_transform_to_isometry(transform)
            .expect("core transform converts to a finite Rapier pose");
        let round_trip =
            rigid_transform_from_isometry(isometry).expect("Rapier pose remains a core transform");

        assert_eq!(round_trip, transform);
    }

    #[test]
    fn fixed_body_applies_the_bin_world_transform_once_and_keeps_cuboids_local() {
        let bin = bin(
            [10.0, -20.0, 30.0],
            [0.0, 0.0, 0.5_f32.sqrt(), 0.5_f32.sqrt()],
            0.5,
            0.1,
        );
        let expected_cuboids = analytic_bin_collision(&bin)
            .expect("finite bin derives")
            .cuboids();
        let world = PhysicsWorld::try_new(&bin).expect("finite bin constructs");

        assert_eq!(
            rigid_transform_from_isometry(
                (*world.rigid_bodies[world.bin_bodies[0]].position()).into()
            )
            .expect("fixed body has a finite pose"),
            bin.world_from_bin()
        );
        for (collider, expected) in world
            .colliders
            .iter()
            .map(|(_, collider)| collider)
            .zip(expected_cuboids)
        {
            assert_eq!(collider.parent(), Some(world.bin_bodies[0]));
            assert_eq!(
                rigid_transform_from_isometry(
                    (*collider
                        .position_wrt_parent()
                        .expect("collider is parented"))
                    .into(),
                )
                .expect("collider has a finite local pose"),
                expected.bin_from_cuboid()
            );
        }
    }

    #[test]
    fn every_analytic_collider_uses_the_configured_contact_coefficients() {
        let bin = bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.73, 0.27);
        let world = PhysicsWorld::try_new(&bin).expect("finite bin constructs");

        for collider in world.colliders.iter().map(|(_, collider)| collider) {
            assert_eq!(collider.friction().to_bits(), 0.73_f32.to_bits());
            assert_eq!(collider.restitution().to_bits(), 0.27_f32.to_bits());
        }
    }

    #[test]
    fn dynamic_body_mass_is_independent_of_part_count_and_colliders_use_contact_properties() {
        let asset_id = AssetId::new("part").expect("nonempty asset ID");
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let properties = ResolvedPhysicalProperties::try_new(2.5, 0.73, 0.27)
            .expect("valid physical properties");
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
        let one_part = vec![part.clone()];
        let two_parts = vec![part.clone(), part];
        let one_part_asset =
            PreparedPhysicsAsset::try_new(&one_part).expect("one-part asset prepares once");
        let two_part_asset =
            PreparedPhysicsAsset::try_new(&two_parts).expect("two-part asset prepares once");
        let mut one_part_world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");
        let mut two_part_world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");

        one_part_world
            .insert_dynamic_bodies(&[DynamicBodySpec::new(
                instance_id,
                &asset_id,
                &one_part_asset,
                bop_core::RigidTransform::identity(),
                properties,
            )])
            .expect("one-part body inserts");
        two_part_world
            .insert_dynamic_bodies(&[DynamicBodySpec::new(
                instance_id,
                &asset_id,
                &two_part_asset,
                bop_core::RigidTransform::identity(),
                properties,
            )])
            .expect("two-part body inserts");

        for world in [&one_part_world, &two_part_world] {
            let metadata = world
                .dynamic_bodies
                .get(&instance_id)
                .expect("inserted body metadata is stored");
            assert_eq!(
                world.rigid_bodies[metadata.body_handle].mass().to_bits(),
                properties.mass_kg().to_bits()
            );
            for &collider_handle in &metadata.collider_handles {
                let collider = &world.colliders[collider_handle];
                assert_eq!(
                    collider.friction().to_bits(),
                    properties.friction().to_bits()
                );
                assert_eq!(
                    collider.restitution().to_bits(),
                    properties.restitution().to_bits()
                );
            }
        }
    }

    #[test]
    fn body_pose_preserves_a_nonfinite_stored_pose_as_a_typed_error() {
        let asset_id = AssetId::new("part").expect("nonempty asset ID");
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let properties = ResolvedPhysicalProperties::try_new(2.5, 0.73, 0.27)
            .expect("valid physical properties");
        let parts = vec![
            ConvexPart::try_new(
                vec![
                    [0.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                ],
                vec![0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3],
            )
            .expect("tetrahedron is checked"),
        ];
        let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
        let mut world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");
        world
            .insert_dynamic_bodies(&[DynamicBodySpec::new(
                instance_id,
                &asset_id,
                &asset,
                bop_core::RigidTransform::identity(),
                properties,
            )])
            .expect("checked body inserts");
        let body_handle = world
            .dynamic_bodies
            .get(&instance_id)
            .expect("inserted body metadata is stored")
            .body_handle;
        world.rigid_bodies[body_handle]
            .set_position(Isometry3::translation(f32::NAN, 0.0, 0.0).into(), true);

        assert!(matches!(
            world.body_pose(instance_id),
            Err(PhysicsWorldError::NonFiniteTransform)
        ));
    }

    #[test]
    fn repeated_bodies_share_the_prepared_shape_allocation() {
        let asset_id = AssetId::new("part").expect("nonempty asset ID");
        let properties = ResolvedPhysicalProperties::try_new(2.5, 0.73, 0.27)
            .expect("valid physical properties");
        let parts = vec![
            ConvexPart::try_new(
                vec![
                    [0.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                ],
                vec![0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3],
            )
            .expect("tetrahedron is checked"),
        ];
        let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
        let mut world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");
        world
            .insert_dynamic_bodies(&[
                DynamicBodySpec::new(
                    InstanceId::new(1).expect("nonzero instance ID"),
                    &asset_id,
                    &asset,
                    bop_core::RigidTransform::identity(),
                    properties,
                ),
                DynamicBodySpec::new(
                    InstanceId::new(2).expect("nonzero instance ID"),
                    &asset_id,
                    &asset,
                    bop_core::RigidTransform::identity(),
                    properties,
                ),
            ])
            .expect("repeated asset bodies insert");

        let first = world.dynamic_bodies[&InstanceId::new(1).expect("nonzero instance ID")]
            .collider_handles[0];
        let second = world.dynamic_bodies[&InstanceId::new(2).expect("nonzero instance ID")]
            .collider_handles[0];
        assert!(std::ptr::eq(
            world.colliders[first].shape(),
            world.colliders[second].shape()
        ));
    }

    #[test]
    fn missing_rapier_body_for_stored_metadata_is_a_typed_invariant_error() {
        let asset_id = AssetId::new("part").expect("nonempty asset ID");
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let properties = ResolvedPhysicalProperties::try_new(2.5, 0.73, 0.27)
            .expect("valid physical properties");
        let parts = vec![
            ConvexPart::try_new(
                vec![
                    [0.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                ],
                vec![0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3],
            )
            .expect("tetrahedron is checked"),
        ];
        let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
        let mut world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");
        world
            .insert_dynamic_bodies(&[DynamicBodySpec::new(
                instance_id,
                &asset_id,
                &asset,
                bop_core::RigidTransform::identity(),
                properties,
            )])
            .expect("body inserts");
        let body_handle = world.dynamic_bodies[&instance_id].body_handle;
        world.rigid_bodies.remove(
            body_handle,
            &mut world.island_manager,
            &mut world.colliders,
            &mut world.impulse_joints,
            &mut world.multibody_joints,
            true,
        );

        assert!(matches!(
            world.body_pose(instance_id),
            Err(PhysicsWorldError::MissingRigidBody { instance_id: id }) if id == instance_id
        ));
    }

    #[test]
    fn compound_body_has_finite_strictly_positive_principal_angular_inertia() {
        let asset_id = AssetId::new("part").expect("nonempty asset ID");
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let properties = ResolvedPhysicalProperties::try_new(2.5, 0.73, 0.27)
            .expect("valid physical properties");
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
        let parts = vec![part.clone(), part];
        let asset = PreparedPhysicsAsset::try_new(&parts).expect("asset prepares once");
        let mut world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");
        world
            .insert_dynamic_bodies(&[DynamicBodySpec::new(
                instance_id,
                &asset_id,
                &asset,
                bop_core::RigidTransform::identity(),
                properties,
            )])
            .expect("compound body inserts");

        let body = &world.rigid_bodies[world.dynamic_bodies[&instance_id].body_handle];
        let principal_inertia = body.mass_properties().local_mprops.principal_inertia();
        assert!(
            principal_inertia
                .to_array()
                .into_iter()
                .all(|value| value.is_finite() && value > 0.0)
        );
    }
}

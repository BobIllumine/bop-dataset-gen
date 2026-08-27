use std::{collections::BTreeMap, sync::Arc};

use bpp_core::{
    AssetId, AttemptId, BinSnapshot, InstanceId, InstanceSnapshot, InvalidBinSnapshotError,
    InvalidPhysicsReportError, InvalidSceneSnapshotError, PhysicsRejectionReason, PhysicsReport,
    RejectionCount, RunPlan, SceneId, SceneSnapshot, SeedDeriver, SeedStage,
};
use thiserror::Error;

use crate::{
    PhysicsWorld, PhysicsWorldError, PlacementPlanner, PlacementPlanningError, PopulationPlanner,
    PopulationPlanningError, PreparedPhysicsAsset,
};

const SEED_FINGERPRINT_DOMAIN: &[u8] = b"bpp.dataset.scene-seeds.v1";

/// Serial deterministic compiler from a validated run plan to physical scenes.
///
/// Each attempt owns a fresh Rapier world. Recoverable physical rejections are
/// counted and retried with attempt-addressed seeds; construction and invariant
/// failures stop immediately as typed errors.
pub struct SerialPhysicsGenerator {
    plan: RunPlan,
    seed_deriver: SeedDeriver,
    population_planner: PopulationPlanner,
    placement_planner: PlacementPlanner,
    bin_snapshot: BinSnapshot,
}

impl SerialPhysicsGenerator {
    /// Compiles immutable planning state and a prepared collision catalog.
    ///
    /// # Errors
    ///
    /// Returns a typed error when referenced collision geometry is missing or
    /// the validated bin cannot be represented by its snapshot contract.
    pub fn try_new(
        plan: &RunPlan,
        prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
    ) -> Result<Self, SceneGenerationError> {
        let population_planner = PopulationPlanner::try_new(plan)?;
        let placement_planner = PlacementPlanner::try_new(plan, prepared_assets)?;
        let bin = plan.bin();
        let bin_snapshot = BinSnapshot::try_new(
            bin.inner_dimensions_m().map(bpp_core::PositiveMeters::get),
            bin.floor_thickness_m().get(),
            bin.wall_thickness_m().get(),
            bin.world_from_bin(),
            bin.material_label(),
        )?;
        Ok(Self {
            plan: plan.clone(),
            seed_deriver: SeedDeriver::new(plan.run().master_seed()),
            population_planner,
            placement_planner,
            bin_snapshot,
        })
    }

    /// Generates one accepted scene within the configured attempt budget.
    ///
    /// # Errors
    ///
    /// Returns [`SceneGenerationError::AttemptsExhausted`] after every allowed
    /// attempt is physically rejected. Deterministic planning, backend, and
    /// snapshot invariant failures are returned immediately.
    pub fn generate_scene(&self, scene_id: SceneId) -> Result<SceneSnapshot, SceneGenerationError> {
        let mut rejections = BTreeMap::new();
        for raw_attempt in 0..self.plan.run().scene_attempt_limit() {
            let attempt_id = AttemptId::new(raw_attempt);
            match self.generate_attempt(scene_id, attempt_id) {
                Ok(accepted) => {
                    return self.build_snapshot(
                        scene_id,
                        attempt_id,
                        raw_attempt + 1,
                        &rejections,
                        &accepted,
                    );
                }
                Err(AttemptError::Rejected(reason)) => increment_rejection(&mut rejections, reason),
                Err(AttemptError::Fatal(error)) => return Err(error),
            }
        }
        Err(SceneGenerationError::AttemptsExhausted {
            scene_id,
            attempts: self.plan.run().scene_attempt_limit(),
            rejection_counts: rejection_counts(&rejections),
        })
    }

    fn generate_attempt(
        &self,
        scene_id: SceneId,
        attempt_id: AttemptId,
    ) -> Result<AcceptedAttempt, AttemptError> {
        let population_seed = self
            .seed_deriver
            .derive(scene_id, attempt_id, SeedStage::Population);
        let placement_seed = self
            .seed_deriver
            .derive(scene_id, attempt_id, SeedStage::Placement);
        let population = self
            .population_planner
            .plan(population_seed)
            .map_err(SceneGenerationError::from)
            .map_err(AttemptError::Fatal)?;
        let placement = match self.placement_planner.place(&population, placement_seed) {
            Ok(placement) => placement,
            Err(PlacementPlanningError::SpawnSaturated { .. }) => {
                return Err(AttemptError::Rejected(
                    PhysicsRejectionReason::SpawnSaturation,
                ));
            }
            Err(error) => return Err(AttemptError::Fatal(error.into())),
        };
        let mut world = PhysicsWorld::try_new(self.plan.bin())
            .map_err(SceneGenerationError::from)
            .map_err(AttemptError::Fatal)?;
        let body_specs = placement.dynamic_body_specs().collect::<Vec<_>>();
        world
            .insert_dynamic_bodies(&body_specs)
            .map_err(SceneGenerationError::from)
            .map_err(AttemptError::Fatal)?;
        let settling = world
            .settle(*self.plan.physics())
            .map_err(SceneGenerationError::from)
            .map_err(AttemptError::Fatal)?;
        if !settling.is_settled() {
            return Err(AttemptError::Rejected(PhysicsRejectionReason::Timeout));
        }
        let metrics = world
            .validate_final_state(self.plan.bin(), *self.plan.physics())
            .map_err(|error| AttemptError::Rejected(error.rejection_reason()))?;
        Ok(AcceptedAttempt {
            placement,
            world,
            steps: settling.steps(),
            metrics,
        })
    }

    fn build_snapshot(
        &self,
        scene_id: SceneId,
        attempt_id: AttemptId,
        attempts: u32,
        rejections: &BTreeMap<PhysicsRejectionReason, u32>,
        accepted: &AcceptedAttempt,
    ) -> Result<SceneSnapshot, SceneGenerationError> {
        let instances = accepted
            .placement
            .instances()
            .iter()
            .map(|instance| {
                let pose = accepted.world.body_pose(instance.instance_id())?.ok_or(
                    SceneGenerationError::MissingFinalBody {
                        instance_id: instance.instance_id(),
                    },
                )?;
                Ok(InstanceSnapshot::new(
                    instance.instance_id(),
                    instance.asset_id().clone(),
                    pose,
                    instance.physical_properties(),
                ))
            })
            .collect::<Result<Vec<_>, SceneGenerationError>>()?;
        let physics = PhysicsReport::try_new(
            attempts,
            u64::from(accepted.steps),
            simulated_seconds(accepted.steps, self.plan.physics().fixed_timestep_s()),
            accepted.metrics.max_linear_speed_mps(),
            accepted.metrics.max_angular_speed_rad_s(),
            accepted.metrics.max_penetration_m(),
            rejection_counts(rejections),
        )?;
        SceneSnapshot::try_new(
            scene_id,
            attempt_id,
            self.seed_fingerprint(scene_id, attempt_id),
            self.bin_snapshot.clone(),
            instances,
            physics,
        )
        .map_err(SceneGenerationError::from)
    }

    fn seed_fingerprint(&self, scene_id: SceneId, attempt_id: AttemptId) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(SEED_FINGERPRINT_DOMAIN);
        for stage in [
            SeedStage::Population,
            SeedStage::Placement,
            SeedStage::Camera,
            SeedStage::Appearance,
        ] {
            hasher.update(&self.seed_deriver.derive(scene_id, attempt_id, stage));
        }
        hasher.finalize().to_hex().to_string()
    }
}

struct AcceptedAttempt {
    placement: crate::PlacementPlan,
    world: PhysicsWorld,
    steps: u32,
    metrics: crate::FinalStateMetrics,
}

enum AttemptError {
    Rejected(PhysicsRejectionReason),
    Fatal(SceneGenerationError),
}

/// Error returned while compiling or generating a serial physical scene.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SceneGenerationError {
    /// Population planning violated a deterministic domain invariant.
    #[error(transparent)]
    Population(#[from] PopulationPlanningError),
    /// Placement compilation or planning failed outside recoverable saturation.
    #[error(transparent)]
    Placement(#[from] PlacementPlanningError),
    /// Rapier world construction, insertion, stepping, or querying failed.
    #[error(transparent)]
    Physics(#[from] PhysicsWorldError),
    /// The checked run-plan bin could not be represented as a snapshot.
    #[error(transparent)]
    BinSnapshot(#[from] InvalidBinSnapshotError),
    /// Accepted-attempt diagnostics could not be represented as a report.
    #[error(transparent)]
    PhysicsReport(#[from] InvalidPhysicsReportError),
    /// Accepted scene data violated a snapshot invariant.
    #[error(transparent)]
    SceneSnapshot(#[from] InvalidSceneSnapshotError),
    /// A placed instance was absent from the final Rapier world.
    #[error("final Rapier world has no body for {instance_id:?}")]
    MissingFinalBody {
        /// Stable identity of the missing body.
        instance_id: InstanceId,
    },
    /// Every configured attempt was rejected without publishing a scene.
    #[error("scene {scene_id:?} exhausted {attempts} attempts: {rejection_counts:?}")]
    AttemptsExhausted {
        /// Requested scene identity.
        scene_id: SceneId,
        /// Exact configured attempt count consumed.
        attempts: u32,
        /// Deterministically ordered reason counts across all attempts.
        rejection_counts: Vec<RejectionCount>,
    },
}

fn increment_rejection(
    rejections: &mut BTreeMap<PhysicsRejectionReason, u32>,
    reason: PhysicsRejectionReason,
) {
    *rejections.entry(reason).or_default() += 1;
}

fn rejection_counts(rejections: &BTreeMap<PhysicsRejectionReason, u32>) -> Vec<RejectionCount> {
    rejections
        .iter()
        .map(|(&reason, &count)| RejectionCount::new(reason, count))
        .collect()
}

#[allow(clippy::cast_possible_truncation)]
fn simulated_seconds(steps: u32, fixed_timestep_s: f32) -> f32 {
    (f64::from(steps) * f64::from(fixed_timestep_s)) as f32
}

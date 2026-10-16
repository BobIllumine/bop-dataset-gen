use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use bpp_assets::ResolvedAssetCatalog;
use bpp_core::{AssetId, RunPlan, SceneId, SceneSnapshot};
use bpp_dataset::write_scene_snapshot;
use bpp_physics_rapier::PreparedPhysicsAsset;
use bpp_render_api::Renderer;

use crate::{
    CancellationToken, ParallelPhysicsExecutor, PipelineError, RapierSimulatorFactory,
    RenderRequestPlanner, SerialDatasetRenderer, SerialGenerationPipelineError, SerialRunReport,
    SerialSnapshotSimulationError, serial_generation::open_dataset_run,
    snapshot_pipeline::open_simulation_corpus,
};

/// Physics-only executor that simulates scenes on parallel workers and publishes snapshots.
///
/// Produces exactly the corpus [`crate::SerialSnapshotSimulator`] produces for
/// the same plan, because every scene is seeded by its ID alone.
pub struct ParallelSnapshotSimulator {
    executor: ParallelPhysicsExecutor,
    factory: RapierSimulatorFactory,
    total_scene_count: u64,
}

impl ParallelSnapshotSimulator {
    /// Reads the configured worker bounds and captures the shared collision catalog.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the configured worker count or queue capacity is zero.
    pub fn try_new(
        plan: &RunPlan,
        prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
    ) -> Result<Self, SerialSnapshotSimulationError> {
        Ok(Self {
            executor: ParallelPhysicsExecutor::from_plan(plan)?,
            factory: RapierSimulatorFactory::new(plan.clone(), prepared_assets),
            total_scene_count: u64::from(plan.run().valid_scene_count()),
        })
    }

    /// Generates every missing scene snapshot on the worker pool.
    ///
    /// Snapshots are written as they arrive, in completion order. Existing
    /// snapshots are resume points and are never replaced. After cancellation,
    /// snapshots already simulated are still written.
    ///
    /// # Errors
    ///
    /// Returns a typed corpus, worker, or scene-generation failure.
    pub fn run(
        &self,
        resolved_config_bytes: &[u8],
        asset_catalog_fingerprint: &str,
        corpus_root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<SerialRunReport, SerialSnapshotSimulationError> {
        let (preexisting_scene_count, existing) = open_simulation_corpus(
            resolved_config_bytes,
            asset_catalog_fingerprint,
            corpus_root,
            self.total_scene_count,
        )?;
        let pending = (0..self.total_scene_count)
            .map(SceneId::new)
            .filter(|scene_id| !existing.contains(scene_id))
            .collect::<Vec<_>>();
        let generated_scene_count = run_with_consumer(
            &self.executor,
            &self.factory,
            &pending,
            cancellation,
            |scene| {
                write_scene_snapshot(corpus_root, &scene)?;
                Ok::<_, SerialSnapshotSimulationError>(true)
            },
        )?;
        Ok(SerialRunReport::new(
            preexisting_scene_count,
            generated_scene_count,
            self.total_scene_count,
            cancellation.is_cancelled(),
        ))
    }
}

/// Generation pipeline with parallel physics feeding one renderer on the calling thread.
///
/// Scenes are committed in completion order. Each commit is atomic and keyed
/// by scene ID, so the finished dataset matches [`crate::SerialGenerationPipeline`].
pub struct ParallelGenerationPipeline<R> {
    executor: ParallelPhysicsExecutor,
    factory: RapierSimulatorFactory,
    requests: RenderRequestPlanner,
    dataset: SerialDatasetRenderer<R>,
    dataset_root: PathBuf,
    total_scene_count: u64,
}

impl<R> ParallelGenerationPipeline<R>
where
    R: Renderer,
{
    /// Reads the configured worker bounds and captures physics and render planning state.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the configured worker count or queue capacity is zero.
    pub fn try_new(
        plan: &RunPlan,
        prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
        renderer: R,
        dataset_root: impl Into<PathBuf>,
    ) -> Result<Self, SerialGenerationPipelineError> {
        let dataset_root = dataset_root.into();
        Ok(Self {
            executor: ParallelPhysicsExecutor::from_plan(plan)?,
            factory: RapierSimulatorFactory::new(plan.clone(), prepared_assets),
            requests: RenderRequestPlanner::new(plan),
            dataset: SerialDatasetRenderer::new(renderer, &dataset_root),
            dataset_root,
            total_scene_count: u64::from(plan.run().valid_scene_count()),
        })
    }

    /// Opens the run manifest, prepares the renderer, and commits every pending scene.
    ///
    /// # Errors
    ///
    /// Returns a typed manifest, resume, preparation, physics, rendering, or flush failure.
    pub fn run_to_completion(
        &mut self,
        resolved_config_bytes: &[u8],
        catalog: &ResolvedAssetCatalog,
    ) -> Result<SerialRunReport, SerialGenerationPipelineError> {
        self.run_until_cancelled(resolved_config_bytes, catalog, &CancellationToken::new())
    }

    /// Runs pending scenes until completion or cooperative cancellation.
    ///
    /// Cancellation stops physics from claiming new scenes and stops the
    /// renderer before its next scene; an in-flight render still reaches its
    /// atomic commit. Simulated snapshots that were never rendered are
    /// discarded and regenerated identically on resume.
    ///
    /// # Errors
    ///
    /// Returns the same typed failures as [`Self::run_to_completion`].
    pub fn run_until_cancelled(
        &mut self,
        resolved_config_bytes: &[u8],
        catalog: &ResolvedAssetCatalog,
        cancellation: &CancellationToken,
    ) -> Result<SerialRunReport, SerialGenerationPipelineError> {
        let (resume, preexisting_scene_count) = open_dataset_run(
            &self.dataset_root,
            resolved_config_bytes,
            catalog,
            self.dataset.renderer().info(),
            self.total_scene_count,
        )?;
        if cancellation.is_cancelled() {
            return Ok(SerialRunReport::new(
                preexisting_scene_count,
                0,
                self.total_scene_count,
                true,
            ));
        }

        self.dataset.prepare(catalog)?;
        let pending = resume.pending_scenes().collect::<Vec<_>>();
        let requests = &self.requests;
        let dataset = &mut self.dataset;
        let generated_scene_count = run_with_consumer::<SerialGenerationPipelineError>(
            &self.executor,
            &self.factory,
            &pending,
            cancellation,
            |scene| {
                if cancellation.is_cancelled() {
                    return Ok(false);
                }
                let request = requests.plan(scene)?;
                dataset
                    .render_and_commit(&request)
                    .map_err(SerialGenerationPipelineError::Render)?;
                Ok(true)
            },
        )?;
        self.dataset.flush()?;

        Ok(SerialRunReport::new(
            preexisting_scene_count,
            generated_scene_count,
            self.total_scene_count,
            cancellation.is_cancelled(),
        ))
    }
}

/// Runs the executor on a background thread and feeds each snapshot to `consume`
/// on the calling thread.
///
/// `consume` returns `Ok(false)` to stop early. Stopping or failing drops the
/// receiver, which makes workers stop at their next send. Returns the number of
/// snapshots consumed. A consumer error takes precedence over the physics error
/// it caused; any other physics error is returned as is.
fn run_with_consumer<E>(
    executor: &ParallelPhysicsExecutor,
    factory: &RapierSimulatorFactory,
    scene_ids: &[SceneId],
    cancellation: &CancellationToken,
    mut consume: impl FnMut(SceneSnapshot) -> Result<bool, E>,
) -> Result<u64, E>
where
    E: From<PipelineError>,
{
    let (sender, receiver) = executor.channel();
    let (physics, consumed) = std::thread::scope(|scope| {
        let physics =
            scope.spawn(move || executor.run_scenes(scene_ids, factory, sender, cancellation));
        let consumed = (|| -> Result<u64, E> {
            let mut count = 0_u64;
            for scene in &receiver {
                if !consume(scene)? {
                    break;
                }
                count += 1;
            }
            Ok(count)
        })();
        drop(receiver);
        let physics = physics
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (physics, consumed)
    });
    let count = consumed?;
    match physics {
        // Only this function drops the receiver, so the consumer stopped first
        // and its own result already describes the outcome.
        Ok(_) | Err(PipelineError::ReceiverDropped) => Ok(count),
        Err(error) => Err(error.into()),
    }
}

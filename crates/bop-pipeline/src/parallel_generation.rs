use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use bop_assets::ResolvedAssetCatalog;
use bop_core::{AssetId, RunPlan, SceneId, SceneSnapshot};
use bop_dataset::{sync_scene_index, sync_snapshot_index, write_scene_snapshots};
use bop_physics_rapier::PreparedPhysicsAsset;
use bop_render_api::Renderer;
use crossbeam_channel::Receiver;

use crate::{
    CancellationToken, GenerationPipelineError, ParallelPhysicsExecutor, PhysicsStageReport,
    PipelineError, PipelineRenderError, RapierSimulatorFactory, RenderRequestPlanner, RunReport,
    SceneCommitObserver, SerialDatasetRenderer, SnapshotRenderError, SnapshotSimulationError,
    render_pool::{dataset_renderers, render_pool},
    serial_generation::open_dataset_run,
    snapshot_pipeline::{open_render_run, open_simulation_corpus},
};

/// Largest number of snapshots committed under one storage flush.
const SNAPSHOT_BATCH_LIMIT: usize = 64;

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
    ) -> Result<Self, SnapshotSimulationError> {
        Ok(Self {
            executor: ParallelPhysicsExecutor::from_plan(plan)?,
            factory: RapierSimulatorFactory::new(plan.clone(), prepared_assets),
            total_scene_count: u64::from(plan.run().valid_scene_count()),
        })
    }

    /// Generates every missing scene snapshot on the worker pool.
    ///
    /// Snapshots are committed in completion order, in batches of whatever has
    /// queued up, with one storage flush per batch. Existing snapshots are
    /// resume points and are never replaced. After cancellation, snapshots
    /// already simulated are still written.
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
    ) -> Result<RunReport, SnapshotSimulationError> {
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
        let generated_scene_count = with_parallel_physics(
            &self.executor,
            &self.factory,
            &pending,
            cancellation,
            |receiver| {
                let mut written = 0_u64;
                while let Ok(first) = receiver.recv() {
                    let mut batch = vec![first];
                    batch.extend(receiver.try_iter().take(SNAPSHOT_BATCH_LIMIT - 1));
                    write_scene_snapshots(corpus_root, &batch)?;
                    tracing::debug!(snapshots = batch.len(), "committed snapshot batch");
                    written += batch.len() as u64;
                }
                Ok::<_, SnapshotSimulationError>(written)
            },
        )?;
        sync_snapshot_index(corpus_root)?;
        Ok(RunReport::new(
            preexisting_scene_count,
            generated_scene_count,
            self.total_scene_count,
            cancellation.is_cancelled(),
        ))
    }
}

/// Generation pipeline with parallel physics feeding a pool of renderers.
///
/// Each renderer runs on its own thread. Scenes are committed in completion
/// order; each commit is atomic and keyed by scene ID, so the finished dataset
/// matches [`crate::SerialGenerationPipeline`].
pub struct ParallelGenerationPipeline<R> {
    executor: ParallelPhysicsExecutor,
    factory: RapierSimulatorFactory,
    requests: RenderRequestPlanner,
    datasets: Vec<SerialDatasetRenderer<R>>,
    dataset_root: PathBuf,
    total_scene_count: u64,
}

impl<R> ParallelGenerationPipeline<R>
where
    R: Renderer,
{
    /// Reads the configured physics bounds and captures one dataset writer per renderer.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the configured worker count or queue
    /// capacity is zero, no renderer is supplied, or renderers disagree on
    /// backend provenance.
    pub fn try_new(
        plan: &RunPlan,
        prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
        renderers: impl IntoIterator<Item = R>,
        dataset_root: impl Into<PathBuf>,
    ) -> Result<Self, GenerationPipelineError> {
        let dataset_root = dataset_root.into();
        let (datasets, _) = dataset_renderers(renderers, &dataset_root)?;
        Ok(Self {
            executor: ParallelPhysicsExecutor::from_plan(plan)?,
            factory: RapierSimulatorFactory::new(plan.clone(), prepared_assets),
            requests: RenderRequestPlanner::new(plan),
            datasets,
            dataset_root,
            total_scene_count: u64::from(plan.run().valid_scene_count()),
        })
    }

    /// Calls `observer` after each scene any renderer commits.
    #[must_use]
    pub fn with_commit_observer(mut self, observer: SceneCommitObserver) -> Self {
        if let Some((last, rest)) = self.datasets.split_last_mut() {
            for dataset in rest {
                dataset.observe_commits(Arc::clone(&observer));
            }
            last.observe_commits(observer);
        }
        self
    }

    /// Opens the run manifest, prepares every renderer, and commits every pending scene.
    ///
    /// # Errors
    ///
    /// Returns a typed manifest, resume, preparation, physics, rendering, or flush failure.
    pub fn run_to_completion(
        &mut self,
        resolved_config_bytes: &[u8],
        catalog: &ResolvedAssetCatalog,
    ) -> Result<RunReport, GenerationPipelineError> {
        self.run_until_cancelled(resolved_config_bytes, catalog, &CancellationToken::new())
    }

    /// Runs pending scenes until completion or cooperative cancellation.
    ///
    /// Cancellation stops physics from claiming new scenes and stops each
    /// renderer before its next scene; in-flight renders still reach their
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
    ) -> Result<RunReport, GenerationPipelineError> {
        let (resume, preexisting_scene_count) = open_dataset_run(
            &self.dataset_root,
            resolved_config_bytes,
            catalog,
            self.datasets[0].renderer().info(),
            self.total_scene_count,
        )?;
        if cancellation.is_cancelled() {
            return Ok(RunReport::new(
                preexisting_scene_count,
                0,
                self.total_scene_count,
                true,
            ));
        }

        for dataset in &mut self.datasets {
            dataset.prepare(catalog)?;
        }
        let pending = resume.pending_scenes().collect::<Vec<_>>();
        let (requests, datasets) = (&self.requests, &mut self.datasets);
        let generated_scene_count = with_parallel_physics(
            &self.executor,
            &self.factory,
            &pending,
            cancellation,
            |receiver| {
                render_pool::<R, GenerationPipelineError>(
                    datasets,
                    requests,
                    receiver,
                    cancellation,
                )
            },
        )?;
        for dataset in &mut self.datasets {
            dataset.flush()?;
        }
        sync_scene_index(&self.dataset_root).map_err(PipelineRenderError::from)?;

        Ok(RunReport::new(
            preexisting_scene_count,
            generated_scene_count,
            self.total_scene_count,
            cancellation.is_cancelled(),
        ))
    }
}

/// Render-only executor that renders a stored snapshot corpus on a pool of renderers.
pub struct ParallelSnapshotRenderer<R> {
    requests: RenderRequestPlanner,
    datasets: Vec<SerialDatasetRenderer<R>>,
    dataset_root: PathBuf,
    queue_capacity: usize,
    total_scene_count: u64,
}

impl<R> ParallelSnapshotRenderer<R>
where
    R: Renderer,
{
    /// Captures render planning and one dataset writer per renderer.
    ///
    /// # Errors
    ///
    /// Returns a typed error when no renderer is supplied or renderers
    /// disagree on backend provenance.
    pub fn try_new(
        plan: &RunPlan,
        renderers: impl IntoIterator<Item = R>,
        dataset_root: impl Into<PathBuf>,
    ) -> Result<Self, SnapshotRenderError> {
        let dataset_root = dataset_root.into();
        let (datasets, _) = dataset_renderers(renderers, &dataset_root)?;
        Ok(Self {
            requests: RenderRequestPlanner::new(plan),
            datasets,
            dataset_root,
            queue_capacity: plan.run().queue_capacity().max(1) as usize,
            total_scene_count: u64::from(plan.run().valid_scene_count()),
        })
    }

    /// Renders a complete canonical snapshot corpus without running physics.
    ///
    /// # Errors
    ///
    /// Returns a typed corpus, resume, planning, rendering, or lifecycle failure.
    pub fn run_to_completion(
        &mut self,
        resolved_config_bytes: &[u8],
        catalog: &ResolvedAssetCatalog,
        snapshot_root: &Path,
    ) -> Result<RunReport, SnapshotRenderError> {
        self.run_until_cancelled(
            resolved_config_bytes,
            catalog,
            snapshot_root,
            &CancellationToken::new(),
        )
    }

    /// Renders pending snapshots until completion or cooperative cancellation.
    ///
    /// # Errors
    ///
    /// Returns the same typed failures as [`Self::run_to_completion`].
    pub fn run_until_cancelled(
        &mut self,
        resolved_config_bytes: &[u8],
        catalog: &ResolvedAssetCatalog,
        snapshot_root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<RunReport, SnapshotRenderError> {
        let (snapshots, resume, preexisting) = open_render_run(
            snapshot_root,
            &self.dataset_root,
            resolved_config_bytes,
            catalog,
            self.datasets[0].renderer().info(),
            self.total_scene_count,
        )?;
        if cancellation.is_cancelled() {
            return Ok(RunReport::new(preexisting, 0, self.total_scene_count, true));
        }
        for dataset in &mut self.datasets {
            dataset.prepare(catalog)?;
        }
        let mut pending = snapshots;
        pending.retain(|scene| resume.pending_scenes().any(|id| id == scene.scene_id()));
        let (sender, receiver) = crossbeam_channel::bounded(self.queue_capacity);
        let (requests, datasets) = (&self.requests, &mut self.datasets);
        let generated = std::thread::scope(|scope| {
            scope.spawn(move || {
                for scene in pending {
                    if cancellation.is_cancelled() || sender.send(scene).is_err() {
                        break;
                    }
                }
            });
            render_pool::<R, SnapshotRenderError>(datasets, requests, receiver, cancellation)
        })?;
        for dataset in &mut self.datasets {
            dataset.flush()?;
        }
        sync_scene_index(&self.dataset_root).map_err(PipelineRenderError::from)?;
        Ok(RunReport::new(
            preexisting,
            generated,
            self.total_scene_count,
            cancellation.is_cancelled(),
        ))
    }
}

/// Runs the physics executor on a background thread while `consume` drains its
/// output on the calling thread.
///
/// `consume` owns the receiver and must drop it before returning, which makes
/// workers stop at their next send. Returns what `consume` returns. A consumer
/// error takes precedence over the physics error it caused; any other physics
/// error is returned as is.
fn with_parallel_physics<E>(
    executor: &ParallelPhysicsExecutor,
    factory: &RapierSimulatorFactory,
    scene_ids: &[SceneId],
    cancellation: &CancellationToken,
    consume: impl FnOnce(Receiver<SceneSnapshot>) -> Result<u64, E>,
) -> Result<u64, E>
where
    E: From<PipelineError>,
{
    let (sender, receiver) = executor.channel();
    let (physics, consumed): (Result<PhysicsStageReport, PipelineError>, _) =
        std::thread::scope(|scope| {
            let physics =
                scope.spawn(move || executor.run_scenes(scene_ids, factory, sender, cancellation));
            let consumed = consume(receiver);
            let physics = physics
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            (physics, consumed)
        });
    let count = consumed?;
    match physics {
        // Only the consumer drops the receiver, so it stopped first and its
        // own result already describes the outcome.
        Ok(_) | Err(PipelineError::ReceiverDropped) => Ok(count),
        Err(error) => Err(error.into()),
    }
}

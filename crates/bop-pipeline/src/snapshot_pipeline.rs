use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use bop_assets::ResolvedAssetCatalog;
use bop_core::{AssetId, BackendInfo, RenderError, RunPlan, SceneId, SceneSnapshot};
use bop_dataset::{
    DatasetManifest, DatasetManifestError, DatasetOpenError, DatasetResumeIndex,
    SnapshotCorpusError, SnapshotCorpusOpenError, open_snapshot_corpus, persist_run_environment,
    read_run_environment, read_snapshot_corpus, sync_scene_index, sync_snapshot_index,
    validate_snapshot_corpus, write_scene_snapshot,
};
use bop_physics_rapier::{PreparedPhysicsAsset, SceneGenerationError, SerialPhysicsGenerator};
use bop_render_api::Renderer;
use thiserror::Error;

use crate::{
    CancellationToken, InvalidParallelPhysicsExecutor, PipelineError, PipelineRenderError,
    RenderRequestPlanner, RenderRequestPlanningError, RendererPoolError, RunReport,
    SerialDatasetRenderer, current_run_environment,
};

/// Serial physics-only executor that atomically publishes renderer-neutral scene snapshots.
pub struct SerialSnapshotSimulator {
    physics: SerialPhysicsGenerator,
    total_scene_count: u64,
}

impl SerialSnapshotSimulator {
    /// Compiles immutable physics planning state.
    ///
    /// # Errors
    ///
    /// Returns a typed error when configured collision assets or bin geometry are invalid.
    pub fn try_new(
        plan: &RunPlan,
        prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
    ) -> Result<Self, SceneGenerationError> {
        Ok(Self {
            physics: SerialPhysicsGenerator::try_new(plan, prepared_assets)?,
            total_scene_count: u64::from(plan.run().valid_scene_count()),
        })
    }

    /// Generates every missing scene snapshot in ascending identity order.
    ///
    /// Cancellation is checked before allocating each scene. Existing canonical
    /// snapshots are treated as completed resume points and are never replaced.
    ///
    /// # Errors
    ///
    /// Returns a typed corpus or scene-generation failure.
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
        let mut generated_scene_count = 0;
        for value in 0..self.total_scene_count {
            if cancellation.is_cancelled() {
                break;
            }
            let scene_id = SceneId::new(value);
            if existing.contains(&scene_id) {
                continue;
            }
            let scene = self.physics.generate_scene(scene_id)?;
            write_scene_snapshot(corpus_root, &scene)?;
            generated_scene_count += 1;
        }
        sync_snapshot_index(corpus_root)?;
        Ok(RunReport::new(
            preexisting_scene_count,
            generated_scene_count,
            self.total_scene_count,
            cancellation.is_cancelled(),
        ))
    }
}

/// Opens or resumes a snapshot corpus and persists its environment.
///
/// Returns the number of snapshots already present and their scene IDs.
pub(crate) fn open_simulation_corpus(
    resolved_config_bytes: &[u8],
    asset_catalog_fingerprint: &str,
    corpus_root: &Path,
    total_scene_count: u64,
) -> Result<(u64, BTreeSet<SceneId>), SnapshotSimulationError> {
    open_snapshot_corpus(
        corpus_root,
        resolved_config_bytes,
        asset_catalog_fingerprint,
        total_scene_count,
    )?;
    persist_run_environment(corpus_root, &current_run_environment())?;
    let existing = if corpus_root.join("scenes").exists() {
        read_snapshot_corpus(corpus_root)?
    } else {
        Vec::new()
    };
    for scene in &existing {
        if scene.scene_id().get() >= total_scene_count {
            return Err(SnapshotSimulationError::SceneOutsideRun {
                scene_id: scene.scene_id(),
                total_scene_count,
            });
        }
    }
    let preexisting_scene_count =
        u64::try_from(existing.len()).map_err(|_| SnapshotSimulationError::SceneCountOverflow)?;
    let existing = existing.into_iter().map(|scene| scene.scene_id()).collect();
    tracing::info!(
        corpus = %corpus_root.display(),
        total_scenes = total_scene_count,
        preexisting_scenes = preexisting_scene_count,
        "opened snapshot corpus"
    );
    Ok((preexisting_scene_count, existing))
}

/// Validates a complete snapshot corpus and opens or resumes the dataset it renders into.
///
/// Returns every snapshot in scene-ID order, the resume index, and the number
/// of scenes committed before this run.
pub(crate) fn open_render_run(
    snapshot_root: &Path,
    dataset_root: &Path,
    resolved_config_bytes: &[u8],
    catalog: &ResolvedAssetCatalog,
    backend: BackendInfo,
    total_scene_count: u64,
) -> Result<(Vec<SceneSnapshot>, DatasetResumeIndex, u64), SnapshotRenderError> {
    validate_snapshot_corpus(
        snapshot_root,
        resolved_config_bytes,
        catalog.fingerprint(),
        total_scene_count,
    )?;
    let _producer_environment = read_run_environment(snapshot_root)?;
    let snapshots = read_snapshot_corpus(snapshot_root)?;
    let found =
        u64::try_from(snapshots.len()).map_err(|_| SnapshotRenderError::SceneCountOverflow)?;
    if found != total_scene_count
        || snapshots
            .iter()
            .enumerate()
            .any(|(index, scene)| scene.scene_id().get() != index as u64)
    {
        return Err(SnapshotRenderError::IncompleteSnapshotCorpus {
            expected: total_scene_count,
            found,
        });
    }
    let manifest = DatasetManifest::try_new(
        resolved_config_bytes,
        catalog.fingerprint(),
        backend,
        total_scene_count,
    )?;
    let resume = DatasetResumeIndex::open_or_create(dataset_root, manifest)?;
    resume.persist_resolved_config(dataset_root, resolved_config_bytes)?;
    persist_run_environment(dataset_root, &current_run_environment())?;
    crate::asset_preparation::persist_catalog_models(dataset_root, catalog)?;
    let preexisting = u64::try_from(resume.committed_scenes().len())
        .map_err(|_| SnapshotRenderError::SceneCountOverflow)?;
    tracing::info!(
        corpus = %snapshot_root.display(),
        dataset = %dataset_root.display(),
        total_scenes = total_scene_count,
        preexisting_scenes = preexisting,
        "opened dataset for render-only run"
    );
    Ok((snapshots, resume, preexisting))
}

/// Serial render-only executor consuming an immutable snapshot corpus.
pub struct SerialSnapshotRenderer<R> {
    requests: RenderRequestPlanner,
    dataset: SerialDatasetRenderer<R>,
    dataset_root: PathBuf,
    total_scene_count: u64,
}

impl<R> SerialSnapshotRenderer<R>
where
    R: Renderer,
{
    /// Captures checked render planning and the owned renderer.
    #[must_use]
    pub fn new(plan: &RunPlan, renderer: R, dataset_root: impl Into<PathBuf>) -> Self {
        let dataset_root = dataset_root.into();
        Self {
            requests: RenderRequestPlanner::new(plan),
            dataset: SerialDatasetRenderer::new(renderer, &dataset_root),
            dataset_root,
            total_scene_count: u64::from(plan.run().valid_scene_count()),
        }
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
    /// Cancellation is observed before renderer preparation and before each
    /// scene. An in-flight render reaches its atomic commit boundary.
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
            self.dataset.renderer().info(),
            self.total_scene_count,
        )?;
        if cancellation.is_cancelled() {
            return Ok(RunReport::new(preexisting, 0, self.total_scene_count, true));
        }
        self.dataset.prepare(catalog)?;
        let mut generated = 0;
        for scene_id in resume.pending_scenes() {
            if cancellation.is_cancelled() {
                break;
            }
            let index = usize::try_from(scene_id.get())
                .map_err(|_| SnapshotRenderError::SceneCountOverflow)?;
            let request = self.requests.plan(snapshots[index].clone())?;
            self.dataset.render_and_commit(&request)?;
            generated += 1;
        }
        self.dataset.flush()?;
        sync_scene_index(&self.dataset_root).map_err(PipelineRenderError::from)?;
        Ok(RunReport::new(
            preexisting,
            generated,
            self.total_scene_count,
            cancellation.is_cancelled(),
        ))
    }
}

/// Failure while generating a physics-only snapshot corpus.
#[derive(Debug, Error)]
pub enum SnapshotSimulationError {
    /// Snapshot corpus identity creation or compatibility failed.
    #[error(transparent)]
    CorpusOpen(#[from] SnapshotCorpusOpenError),
    /// Environment provenance could not be persisted compatibly.
    #[error(transparent)]
    DatasetOpen(#[from] DatasetOpenError),
    /// Physics generation failed.
    #[error(transparent)]
    Physics(#[from] SceneGenerationError),
    /// Snapshot storage failed.
    #[error(transparent)]
    Corpus(#[from] SnapshotCorpusError),
    /// The parallel physics stage failed.
    #[error(transparent)]
    ParallelPhysics(#[from] PipelineError),
    /// The configured worker count or queue capacity is zero.
    #[error(transparent)]
    InvalidExecutor(#[from] InvalidParallelPhysicsExecutor),
    /// An existing scene is outside the configured run domain.
    #[error("snapshot scene {scene_id:?} is outside total count {total_scene_count}")]
    SceneOutsideRun {
        /// Invalid scene identity.
        scene_id: SceneId,
        /// Exclusive run bound.
        total_scene_count: u64,
    },
    /// A platform collection length exceeded the protocol count.
    #[error("snapshot count cannot be represented as u64")]
    SceneCountOverflow,
}

/// Failure while rendering an existing snapshot corpus.
#[derive(Debug, Error)]
pub enum SnapshotRenderError {
    /// Snapshot corpus identity is missing or incompatible.
    #[error(transparent)]
    CorpusOpen(#[from] SnapshotCorpusOpenError),
    /// Snapshot storage or parsing failed.
    #[error(transparent)]
    Corpus(#[from] SnapshotCorpusError),
    /// The corpus is not exactly the configured scene-ID domain.
    #[error("snapshot corpus is incomplete: expected {expected} scenes, found {found}")]
    IncompleteSnapshotCorpus {
        /// Configured scene count.
        expected: u64,
        /// Parsed scene count.
        found: u64,
    },
    /// Dataset manifest construction failed.
    #[error(transparent)]
    Manifest(#[from] DatasetManifestError),
    /// Dataset creation or resume failed.
    #[error(transparent)]
    DatasetOpen(#[from] DatasetOpenError),
    /// Request planning failed.
    #[error(transparent)]
    Request(#[from] RenderRequestPlanningError),
    /// Rendering or atomic commit failed.
    #[error(transparent)]
    Render(#[from] PipelineRenderError),
    /// Renderer preparation or flush failed.
    #[error(transparent)]
    RendererLifecycle(#[from] RenderError),
    /// The supplied renderers cannot form one pool.
    #[error(transparent)]
    RendererPool(#[from] RendererPoolError),
    /// A platform collection length exceeded the protocol count.
    #[error("scene count cannot be represented on this platform")]
    SceneCountOverflow,
}

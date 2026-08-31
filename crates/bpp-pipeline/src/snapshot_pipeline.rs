use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use bpp_assets::ResolvedAssetCatalog;
use bpp_core::{AssetId, RenderError, Renderer, RunPlan, SceneId};
use bpp_dataset::{
    DatasetManifest, DatasetManifestError, DatasetOpenError, DatasetResumeIndex, SceneStagingArea,
    SnapshotCorpusError, SnapshotCorpusOpenError, open_snapshot_corpus, read_snapshot_corpus,
    write_scene_snapshot,
};
use bpp_physics_rapier::{PreparedPhysicsAsset, SceneGenerationError, SerialPhysicsGenerator};
use thiserror::Error;

use crate::{
    CancellationToken, PipelineRenderError, RenderRequestPlanner, RenderRequestPlanningError,
    SerialDatasetRenderer, SerialRunReport,
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
    ) -> Result<SerialRunReport, SerialSnapshotSimulationError> {
        open_snapshot_corpus(
            corpus_root,
            resolved_config_bytes,
            asset_catalog_fingerprint,
            self.total_scene_count,
        )?;
        let existing = if corpus_root.join("scenes").exists() {
            read_snapshot_corpus(corpus_root)?
        } else {
            Vec::new()
        };
        for scene in &existing {
            if scene.scene_id().get() >= self.total_scene_count {
                return Err(SerialSnapshotSimulationError::SceneOutsideRun {
                    scene_id: scene.scene_id(),
                    total_scene_count: self.total_scene_count,
                });
            }
        }
        let preexisting_scene_count = u64::try_from(existing.len())
            .map_err(|_| SerialSnapshotSimulationError::SceneCountOverflow)?;
        let existing = existing
            .into_iter()
            .map(|scene| scene.scene_id())
            .collect::<std::collections::BTreeSet<_>>();
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
        Ok(SerialRunReport::new(
            preexisting_scene_count,
            generated_scene_count,
            self.total_scene_count,
            cancellation.is_cancelled(),
        ))
    }
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
    R: Renderer<AssetCatalog = ResolvedAssetCatalog, StagingArea = SceneStagingArea>,
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
    ) -> Result<SerialRunReport, SerialSnapshotRenderError> {
        let snapshots = read_snapshot_corpus(snapshot_root)?;
        let found = u64::try_from(snapshots.len())
            .map_err(|_| SerialSnapshotRenderError::SceneCountOverflow)?;
        if found != self.total_scene_count
            || snapshots
                .iter()
                .enumerate()
                .any(|(index, scene)| scene.scene_id().get() != index as u64)
        {
            return Err(SerialSnapshotRenderError::IncompleteSnapshotCorpus {
                expected: self.total_scene_count,
                found,
            });
        }
        let manifest = DatasetManifest::try_new(
            resolved_config_bytes,
            catalog.fingerprint(),
            self.dataset.renderer().info(),
            self.total_scene_count,
        )?;
        let resume = DatasetResumeIndex::open_or_create(&self.dataset_root, manifest)?;
        resume.persist_resolved_config(&self.dataset_root, resolved_config_bytes)?;
        let preexisting = u64::try_from(resume.committed_scenes().len())
            .map_err(|_| SerialSnapshotRenderError::SceneCountOverflow)?;
        self.dataset.prepare(catalog)?;
        let mut generated = 0;
        for scene_id in resume.pending_scenes() {
            let index = usize::try_from(scene_id.get())
                .map_err(|_| SerialSnapshotRenderError::SceneCountOverflow)?;
            let request = self.requests.plan(snapshots[index].clone())?;
            self.dataset.render_and_commit(&request)?;
            generated += 1;
        }
        self.dataset.flush()?;
        Ok(SerialRunReport::new(
            preexisting,
            generated,
            self.total_scene_count,
            false,
        ))
    }
}

/// Failure while generating a physics-only snapshot corpus.
#[derive(Debug, Error)]
pub enum SerialSnapshotSimulationError {
    /// Snapshot corpus identity creation or compatibility failed.
    #[error(transparent)]
    CorpusOpen(#[from] SnapshotCorpusOpenError),
    /// Physics generation failed.
    #[error(transparent)]
    Physics(#[from] SceneGenerationError),
    /// Snapshot storage failed.
    #[error(transparent)]
    Corpus(#[from] SnapshotCorpusError),
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
pub enum SerialSnapshotRenderError {
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
    /// A platform collection length exceeded the protocol count.
    #[error("scene count cannot be represented on this platform")]
    SceneCountOverflow,
}

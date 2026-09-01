use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use bpp_assets::ResolvedAssetCatalog;
use bpp_core::{AssetId, RenderError, Renderer, RunPlan, SceneId};
use bpp_dataset::{
    DatasetManifest, DatasetManifestError, DatasetOpenError, DatasetResumeIndex, SceneStagingArea,
    persist_run_environment,
};
use bpp_physics_rapier::{PreparedPhysicsAsset, SceneGenerationError, SerialPhysicsGenerator};
use thiserror::Error;

use crate::{
    CancellationToken, CompletedScene, PipelineRenderError, RenderRequestPlanner,
    RenderRequestPlanningError, SerialDatasetRenderer, current_run_environment,
};

/// Single-threaded reference composition of physics, request planning, rendering, and commit.
pub struct SerialGenerationPipeline<R> {
    physics: SerialPhysicsGenerator,
    requests: RenderRequestPlanner,
    dataset: SerialDatasetRenderer<R>,
    dataset_root: PathBuf,
    total_scene_count: u64,
}

impl<R> SerialGenerationPipeline<R>
where
    R: Renderer<AssetCatalog = ResolvedAssetCatalog, StagingArea = SceneStagingArea>,
{
    /// Compiles immutable physics and render planning state.
    ///
    /// # Errors
    ///
    /// Returns a typed physics-planning failure when a configured asset has no
    /// prepared collision representation or the bin cannot be represented.
    pub fn try_new(
        plan: &RunPlan,
        prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
        renderer: R,
        dataset_root: impl Into<PathBuf>,
    ) -> Result<Self, SerialGenerationPipelineError> {
        let dataset_root = dataset_root.into();
        Ok(Self {
            physics: SerialPhysicsGenerator::try_new(plan, prepared_assets)?,
            requests: RenderRequestPlanner::new(plan),
            dataset: SerialDatasetRenderer::new(renderer, &dataset_root),
            dataset_root,
            total_scene_count: u64::from(plan.run().valid_scene_count()),
        })
    }

    /// Prepares the owned renderer once from the immutable visual catalog.
    ///
    /// # Errors
    ///
    /// Returns the selected renderer's preparation failure.
    pub fn prepare(&mut self, catalog: &ResolvedAssetCatalog) -> Result<(), RenderError> {
        self.dataset.prepare(catalog)
    }

    /// Generates, renders, validates, and atomically commits one scene.
    ///
    /// No render staging tree is created until physics and deterministic camera
    /// planning have both succeeded.
    ///
    /// # Errors
    ///
    /// Returns a typed physics, request-planning, render, validation, or commit failure.
    pub fn generate_and_commit(
        &mut self,
        scene_id: SceneId,
    ) -> Result<CompletedScene, SerialGenerationPipelineError> {
        let scene = self.physics.generate_scene(scene_id)?;
        let request = self.requests.plan(scene)?;
        self.dataset
            .render_and_commit(&request)
            .map_err(SerialGenerationPipelineError::Render)
    }

    /// Flushes the owned renderer at a durability or shutdown boundary.
    ///
    /// # Errors
    ///
    /// Returns the renderer's typed flush failure.
    pub fn flush(&mut self) -> Result<(), RenderError> {
        self.dataset.flush()
    }

    /// Opens the immutable run manifest, prepares the renderer, and commits every pending scene.
    ///
    /// `resolved_config_bytes` must be the exact persisted resolved configuration. Its
    /// versioned fingerprint, the visual catalog fingerprint, renderer provenance, and
    /// requested scene count form the resume identity. Existing committed scenes are skipped.
    ///
    /// # Errors
    ///
    /// Returns a typed manifest, resume, preparation, generation, rendering, or flush failure.
    pub fn run_to_completion(
        &mut self,
        resolved_config_bytes: &[u8],
        catalog: &ResolvedAssetCatalog,
    ) -> Result<SerialRunReport, SerialGenerationPipelineError> {
        self.run_until_cancelled(resolved_config_bytes, catalog, &CancellationToken::new())
    }

    /// Runs pending scenes serially until completion or cooperative cancellation.
    ///
    /// Cancellation is observed before renderer preparation and before each new
    /// scene. An in-flight scene always reaches its atomic commit boundary.
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
        let manifest = DatasetManifest::try_new(
            resolved_config_bytes,
            catalog.fingerprint(),
            self.dataset.renderer().info(),
            self.total_scene_count,
        )?;
        let resume = DatasetResumeIndex::open_or_create(&self.dataset_root, manifest)?;
        resume.persist_resolved_config(&self.dataset_root, resolved_config_bytes)?;
        persist_run_environment(&self.dataset_root, &current_run_environment())?;
        let preexisting_scene_count = u64::try_from(resume.committed_scenes().len())
            .map_err(|_| SerialGenerationPipelineError::SceneCountOverflow)?;

        if cancellation.is_cancelled() {
            return Ok(SerialRunReport {
                preexisting_scene_count,
                generated_scene_count: 0,
                total_scene_count: self.total_scene_count,
                was_cancelled: true,
            });
        }

        self.prepare(catalog)?;
        let mut generated_scene_count = 0_u64;
        for scene_id in resume.pending_scenes() {
            if cancellation.is_cancelled() {
                break;
            }
            self.generate_and_commit(scene_id)?;
            generated_scene_count += 1;
        }
        self.flush()?;

        Ok(SerialRunReport {
            preexisting_scene_count,
            generated_scene_count,
            total_scene_count: self.total_scene_count,
            was_cancelled: cancellation.is_cancelled(),
        })
    }
}

/// Deterministic completion counts for one serial run invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SerialRunReport {
    preexisting_scene_count: u64,
    generated_scene_count: u64,
    total_scene_count: u64,
    was_cancelled: bool,
}

impl SerialRunReport {
    pub(crate) const fn new(
        preexisting_scene_count: u64,
        generated_scene_count: u64,
        total_scene_count: u64,
        was_cancelled: bool,
    ) -> Self {
        Self {
            preexisting_scene_count,
            generated_scene_count,
            total_scene_count,
            was_cancelled,
        }
    }

    /// Returns scenes already atomically committed when the run opened.
    #[must_use]
    pub const fn preexisting_scene_count(self) -> u64 {
        self.preexisting_scene_count
    }

    /// Returns scenes atomically committed by this invocation.
    #[must_use]
    pub const fn generated_scene_count(self) -> u64 {
        self.generated_scene_count
    }

    /// Returns the immutable requested scene count.
    #[must_use]
    pub const fn total_scene_count(self) -> u64 {
        self.total_scene_count
    }

    /// Reports whether cancellation was observed during this invocation.
    #[must_use]
    pub const fn was_cancelled(self) -> bool {
        self.was_cancelled
    }
}

/// Failure in the serial reference generation composition.
#[derive(Debug, Error)]
pub enum SerialGenerationPipelineError {
    /// Physical planning, simulation, or final-state validation failed.
    #[error(transparent)]
    Physics(#[from] SceneGenerationError),
    /// Camera or render-request planning failed.
    #[error(transparent)]
    Request(#[from] RenderRequestPlanningError),
    /// Rendering, canonical validation, or atomic commit failed.
    #[error(transparent)]
    Render(#[from] PipelineRenderError),
    /// The immutable dataset manifest was invalid.
    #[error(transparent)]
    Manifest(#[from] DatasetManifestError),
    /// Dataset creation or compatible resume scanning failed.
    #[error(transparent)]
    DatasetOpen(#[from] DatasetOpenError),
    /// Renderer preparation or flushing failed.
    #[error(transparent)]
    RendererLifecycle(#[from] RenderError),
    /// A platform collection length could not fit the protocol count.
    #[error("scene count cannot be represented as u64")]
    SceneCountOverflow,
}

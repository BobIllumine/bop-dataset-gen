use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use bpp_assets::ResolvedAssetCatalog;
use bpp_core::{AssetId, RenderError, Renderer, RunPlan, SceneId};
use bpp_dataset::SceneStagingArea;
use bpp_physics_rapier::{PreparedPhysicsAsset, SceneGenerationError, SerialPhysicsGenerator};
use thiserror::Error;

use crate::{
    CompletedScene, PipelineRenderError, RenderRequestPlanner, RenderRequestPlanningError,
    SerialDatasetRenderer,
};

/// Single-threaded reference composition of physics, request planning, rendering, and commit.
pub struct SerialGenerationPipeline<R> {
    physics: SerialPhysicsGenerator,
    requests: RenderRequestPlanner,
    dataset: SerialDatasetRenderer<R>,
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
        Ok(Self {
            physics: SerialPhysicsGenerator::try_new(plan, prepared_assets)?,
            requests: RenderRequestPlanner::new(plan),
            dataset: SerialDatasetRenderer::new(renderer, dataset_root),
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
}

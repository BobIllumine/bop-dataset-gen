//! The renderer boundary every backend implements.
//!
//! This crate sits above `bop-assets` and `bop-dataset`, so the trait names the
//! concrete asset catalog and staging area instead of abstracting over them.
//!
//! A renderer receives immutable requests, writes frames only into the staging
//! area it is given and only through the `bop-dataset` encoders, and never
//! falls back to another backend on its own.
//!
//! Guide: <https://bobillumine.github.io/bop-dataset-gen/contributing/renderer-backends.html>

use bop_assets::ResolvedAssetCatalog;
use bop_core::{BackendInfo, RenderError, RenderReport, RenderSceneRequest, RendererCapabilities};
use bop_dataset::SceneStagingArea;

/// Synchronous, scene-batched renderer owned mutably by one execution worker.
///
/// The trait is object-safe. Implementations must not perform implicit
/// backend fallback.
pub trait Renderer: Send {
    /// Returns immutable backend identity and provenance.
    fn info(&self) -> BackendInfo;

    /// Returns channels and scene batching limits supported by this instance.
    fn capabilities(&self) -> RendererCapabilities;

    /// Prepares persistent backend resources for the complete asset catalog.
    ///
    /// # Errors
    ///
    /// Returns a typed capability, resource, content, or backend error without
    /// silently selecting a different renderer.
    fn prepare(&mut self, assets: &ResolvedAssetCatalog) -> Result<(), RenderError>;

    /// Renders every camera in one immutable request into an isolated staging area.
    ///
    /// # Errors
    ///
    /// Returns a typed error without publishing a partial dataset scene.
    fn render_scene(
        &mut self,
        request: &RenderSceneRequest,
        staging: &SceneStagingArea,
    ) -> Result<RenderReport, RenderError>;

    /// Completes pending backend work before shutdown or a durability boundary.
    ///
    /// # Errors
    ///
    /// Returns a typed backend error if queued work cannot be completed.
    fn flush(&mut self) -> Result<(), RenderError>;
}

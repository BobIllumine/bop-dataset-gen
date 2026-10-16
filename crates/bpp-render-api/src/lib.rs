//! The renderer boundary every backend implements.
//!
//! This crate sits above `bpp-assets` and `bpp-dataset`, so the trait names the
//! concrete asset catalog and staging area instead of abstracting over them.

use bpp_assets::ResolvedAssetCatalog;
use bpp_core::{BackendInfo, RenderError, RenderReport, RenderSceneRequest, RendererCapabilities};
use bpp_dataset::SceneStagingArea;

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

use thiserror::Error;

use crate::{BackendInfo, RenderChannels, RenderReport, RenderSceneRequest, RendererCapabilities};

/// Synchronous, scene-batched renderer owned mutably by one execution worker.
///
/// Associated boundary types keep this core trait independent of asset and
/// filesystem crates while remaining object-safe once those types are fixed by
/// the pipeline. Implementations must not perform implicit backend fallback.
pub trait Renderer: Send {
    /// Shared renderer-neutral asset catalog type.
    type AssetCatalog: ?Sized;
    /// Per-scene staging-area type.
    type StagingArea: ?Sized;

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
    fn prepare(&mut self, assets: &Self::AssetCatalog) -> Result<(), RenderError>;

    /// Renders every camera in one immutable request into an isolated staging area.
    ///
    /// # Errors
    ///
    /// Returns a typed error without publishing a partial dataset scene.
    fn render_scene(
        &mut self,
        request: &RenderSceneRequest,
        staging: &Self::StagingArea,
    ) -> Result<RenderReport, RenderError>;

    /// Completes pending backend work before shutdown or a durability boundary.
    ///
    /// # Errors
    ///
    /// Returns a typed backend error if queued work cannot be completed.
    fn flush(&mut self) -> Result<(), RenderError>;
}

/// Renderer failure category shared by in-process and supervised backends.
#[derive(Clone, Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum RenderError {
    /// Requested channels or camera count exceed declared capabilities.
    #[error(
        "renderer capabilities {supported:?} do not support channels {required_channels:?} for {camera_count} cameras"
    )]
    UnsupportedCapabilities {
        /// Requested nonempty channel set.
        required_channels: RenderChannels,
        /// Requested camera count.
        camera_count: u32,
        /// Capabilities reported by the selected renderer.
        supported: RendererCapabilities,
    },
    /// Rendering was attempted before successful asset preparation.
    #[error("renderer assets have not been prepared")]
    NotPrepared,
    /// Required runtime, device, executable, or codec is unavailable.
    #[error("renderer backend unavailable: {message}")]
    BackendUnavailable {
        /// Stable backend diagnostic.
        message: String,
    },
    /// Input geometry, material, camera, or appearance content is unsupported.
    #[error("invalid renderer content: {message}")]
    InvalidContent {
        /// Stable content diagnostic.
        message: String,
    },
    /// A bounded renderer operation exceeded its deadline.
    #[error("renderer operation timed out: {message}")]
    Timeout {
        /// Stable timeout diagnostic.
        message: String,
    },
    /// A supervised renderer worker exited unexpectedly and may be restarted once.
    #[error("renderer worker crashed: {message}")]
    WorkerCrashed {
        /// Stable crash diagnostic, normally including exit status.
        message: String,
    },
    /// A versioned renderer protocol message was malformed or incompatible.
    #[error("renderer protocol failure: {message}")]
    Protocol {
        /// Stable protocol diagnostic.
        message: String,
    },
    /// Renderer staging input/output failed.
    #[error("renderer I/O failure during {operation}: {message}")]
    Io {
        /// Stable operation label.
        operation: String,
        /// Stable underlying diagnostic.
        message: String,
    },
    /// A backend invariant failed without a safe recovery path.
    #[error("internal renderer invariant failed: {message}")]
    Internal {
        /// Stable invariant diagnostic.
        message: String,
    },
}

impl RenderError {
    /// Reports whether the pipeline may perform the single permitted worker restart.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::WorkerCrashed { .. })
    }
}

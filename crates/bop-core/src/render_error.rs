use thiserror::Error;

use crate::{RenderChannels, RendererCapabilities};

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

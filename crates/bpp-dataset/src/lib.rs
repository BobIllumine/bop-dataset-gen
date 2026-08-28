//! Canonical dataset storage, validation, resume, and export.

mod frames;
mod staging;

pub use frames::{DepthFrame, FrameValidationError, FrameWriteError, InstanceFrame, RgbFrame};
pub use staging::{
    InvalidSceneArtifactRequirements, SceneArtifactRequirements, SceneCommitError, SceneStagingArea,
};

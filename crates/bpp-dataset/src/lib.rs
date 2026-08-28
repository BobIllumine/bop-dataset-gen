//! Canonical dataset storage, validation, resume, and export.

mod frames;
mod resume;
mod staging;
mod validation;

pub use frames::{DepthFrame, FrameValidationError, FrameWriteError, InstanceFrame, RgbFrame};
pub use resume::{DatasetManifest, DatasetManifestError, DatasetOpenError, DatasetResumeIndex};
pub use staging::{
    InvalidSceneArtifactRequirements, SceneArtifactRequirements, SceneCommitError, SceneStagingArea,
};
pub use validation::{DatasetValidationError, validate_and_write_checksums};

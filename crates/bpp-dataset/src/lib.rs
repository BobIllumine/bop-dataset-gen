//! Canonical dataset storage, validation, resume, and export.

mod frames;
mod resume;
mod snapshot_corpus;
mod staging;
mod validation;

pub use frames::{DepthFrame, FrameValidationError, FrameWriteError, InstanceFrame, RgbFrame};
pub use resume::{DatasetManifest, DatasetManifestError, DatasetOpenError, DatasetResumeIndex};
pub use snapshot_corpus::{
    SnapshotCorpusError, SnapshotCorpusOpenError, open_snapshot_corpus, read_snapshot_corpus,
    validate_snapshot_corpus, write_scene_snapshot,
};
pub use staging::{
    InvalidSceneArtifactRequirements, SceneArtifactRequirements, SceneCommitError, SceneStagingArea,
};
pub use validation::{
    DatasetValidationError, DatasetValidationReport, validate_and_write_checksums, validate_dataset,
};

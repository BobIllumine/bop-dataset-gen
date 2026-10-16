//! Canonical dataset storage, validation, resume, and export.

mod durability;
mod frames;
mod resume;
mod snapshot_corpus;
mod staging;
mod validation;

pub use frames::{DepthFrame, FrameValidationError, FrameWriteError, InstanceFrame, RgbFrame};
pub use resume::{
    DatasetManifest, DatasetManifestError, DatasetOpenError, DatasetResumeIndex,
    InvalidRunEnvironment, RunEnvironment, persist_run_environment, read_run_environment,
    validate_run_environment,
};
pub use snapshot_corpus::{
    SnapshotCorpusError, SnapshotCorpusOpenError, open_snapshot_corpus, read_snapshot_corpus,
    sync_snapshot_index, validate_snapshot_corpus, write_scene_snapshot, write_scene_snapshots,
};
pub use staging::{
    InvalidSceneArtifactRequirements, SceneArtifactRequirements, SceneCommitError,
    SceneStagingArea, sync_scene_index,
};
pub use validation::{
    DatasetValidationError, DatasetValidationReport, validate_and_write_checksums, validate_dataset,
};

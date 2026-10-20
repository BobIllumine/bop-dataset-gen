//! Canonical dataset storage, validation, resume, and export.

mod bop;
mod durability;
mod frames;
mod models;
mod resume;
mod snapshot_corpus;
mod staging;
mod validation;

pub use bop::{
    BOP_SPLIT, BopExportError, BopExportOptions, BopReport, BopValidationError, export_bop,
    validate_bop,
};
pub use frames::{DepthFrame, FrameValidationError, FrameWriteError, InstanceFrame, RgbFrame};
pub use models::{
    CanonicalModel, CanonicalModelError, InvalidCanonicalModel, persist_canonical_models,
    read_canonical_models,
};
pub use resume::{
    DatasetManifest, DatasetManifestError, DatasetOpenError, DatasetResumeIndex,
    InvalidRunEnvironment, RunEnvironment, fingerprint_config, persist_run_environment,
    read_run_environment, validate_run_environment,
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

//! Bounded orchestration for physics, rendering, and dataset commits.

mod asset_preparation;
mod benchmark;
mod cancellation;
mod parallel_generation;
mod parallel_physics;
mod request_planner;
mod serial_generation;
mod snapshot_pipeline;

pub use asset_preparation::{AssetPreparationError, PreparedAssetCatalog, prepare_asset_catalog};
pub use benchmark::{InvalidRenderBenchmarkSummary, RenderBenchmarkSummary};
pub use cancellation::CancellationToken;
pub use parallel_generation::{ParallelGenerationPipeline, ParallelSnapshotSimulator};
pub use parallel_physics::{
    InvalidParallelPhysicsExecutor, ParallelPhysicsExecutor, PhysicsSimulator, PhysicsStageReport,
    PipelineError, RapierSimulator, RapierSimulatorFactory, SimulatorFactory,
};
pub use request_planner::{RenderRequestPlanner, RenderRequestPlanningError};
pub use serial_generation::{
    SerialGenerationPipeline, SerialGenerationPipelineError, SerialRunReport,
};
pub use snapshot_pipeline::{
    SerialSnapshotRenderError, SerialSnapshotRenderer, SerialSnapshotSimulationError,
    SerialSnapshotSimulator,
};

fn current_run_environment() -> bpp_dataset::RunEnvironment {
    let mut features = Vec::new();
    if cfg!(feature = "fast") {
        features.push("fast".to_owned());
    }
    if cfg!(feature = "reproducible") {
        features.push("reproducible".to_owned());
    }
    bpp_dataset::RunEnvironment::try_new(
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        features,
    )
    .expect("compile-time run environment is nonblank")
}

use std::{
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
};

use bpp_assets::ResolvedAssetCatalog;
use bpp_core::{RenderError, RenderReport, RenderSceneRequest};
use bpp_dataset::{
    DatasetValidationError, SceneArtifactRequirements, SceneCommitError, SceneStagingArea,
    validate_and_write_checksums,
};
use bpp_render_api::Renderer;
use thiserror::Error;

/// Single-owner synchronous renderer orchestration with transactional output.
pub struct SerialDatasetRenderer<R> {
    renderer: R,
    dataset_root: PathBuf,
}

impl<R> SerialDatasetRenderer<R>
where
    R: Renderer,
{
    /// Creates a serial renderer pipeline targeting `dataset_root`.
    #[must_use]
    pub fn new(renderer: R, dataset_root: impl Into<PathBuf>) -> Self {
        Self {
            renderer,
            dataset_root: dataset_root.into(),
        }
    }

    /// Prepares the selected renderer from the immutable resolved catalog.
    ///
    /// # Errors
    ///
    /// Returns the renderer's typed preparation failure.
    pub fn prepare(&mut self, catalog: &ResolvedAssetCatalog) -> Result<(), RenderError> {
        self.renderer.prepare(catalog)
    }

    /// Renders, validates, and atomically commits one complete scene.
    ///
    /// Capability checks happen before staging is created. Any later error
    /// drops and cleans the private staging tree without publishing a scene.
    ///
    /// # Errors
    ///
    /// Returns a typed capability, renderer, metadata, report, or commit error.
    pub fn render_and_commit(
        &mut self,
        request: &RenderSceneRequest,
    ) -> Result<CompletedScene, PipelineRenderError> {
        let camera_count = u32::try_from(request.cameras().len()).map_err(|_| {
            PipelineRenderError::InvalidReport {
                field: RenderReportField::CameraCount,
            }
        })?;
        self.renderer
            .capabilities()
            .require(request.required_channels(), camera_count)?;
        let staging = SceneStagingArea::create(&self.dataset_root, request.scene().scene_id())
            .map_err(|source| PipelineRenderError::StagingIo {
                path: self.dataset_root.clone(),
                source,
            })?;
        write_json(&staging.scene_metadata_path(), request.scene())?;
        for camera in request.cameras() {
            staging
                .prepare_camera(camera.camera_id())
                .map_err(|source| PipelineRenderError::StagingIo {
                    path: staging.camera_dir(camera.camera_id()),
                    source,
                })?;
            write_json(&staging.frame_metadata_path(camera.camera_id()), camera)?;
        }

        let report = self.renderer.render_scene(request, &staging)?;
        validate_report(&report, request, &self.renderer.info(), camera_count)?;
        validate_and_write_checksums(&staging, request)?;
        let requirements = SceneArtifactRequirements::try_new(
            request
                .cameras()
                .iter()
                .map(|camera| camera.camera_id())
                .collect(),
            request.required_channels(),
        )
        .map_err(|_| PipelineRenderError::InvalidReport {
            field: RenderReportField::CameraCount,
        })?;
        let scene_path = staging.commit(&requirements)?;
        Ok(CompletedScene { report, scene_path })
    }

    /// Flushes pending backend work at a durability or shutdown boundary.
    ///
    /// # Errors
    ///
    /// Returns the renderer's typed flush failure.
    pub fn flush(&mut self) -> Result<(), RenderError> {
        self.renderer.flush()
    }

    /// Returns the owned renderer for inspection or shutdown handling.
    #[must_use]
    pub const fn renderer(&self) -> &R {
        &self.renderer
    }
}

/// One successfully rendered and atomically published scene.
#[derive(Clone, Debug, PartialEq)]
pub struct CompletedScene {
    report: RenderReport,
    scene_path: PathBuf,
}

impl CompletedScene {
    /// Returns the validated renderer report.
    #[must_use]
    pub const fn report(&self) -> &RenderReport {
        &self.report
    }

    /// Returns the canonical committed scene directory.
    #[must_use]
    pub fn scene_path(&self) -> &Path {
        &self.scene_path
    }
}

/// Renderer report field that disagreed with the submitted request/backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderReportField {
    /// Scene identity differed.
    SceneId,
    /// Backend provenance differed from the active renderer.
    Backend,
    /// Camera count differed.
    CameraCount,
    /// Output channels differed.
    Channels,
}

/// Failure in the render-to-dataset transaction.
#[derive(Debug, Error)]
pub enum PipelineRenderError {
    /// Renderer capability or execution failure.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// Private staging metadata could not be written.
    #[error("staging I/O failed at {path}: {source}")]
    StagingIo {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Metadata JSON serialization failed.
    #[error("staging metadata serialization failed: {0}")]
    SerializeMetadata(serde_json::Error),
    /// Renderer report contradicted immutable request/backend data.
    #[error("renderer report field {field:?} contradicted the render request")]
    InvalidReport {
        /// Contradictory report field.
        field: RenderReportField,
    },
    /// Staged scene validation or atomic publication failed.
    #[error(transparent)]
    Commit(#[from] SceneCommitError),
    /// A rendered frame violated the canonical dataset contract.
    #[error(transparent)]
    Validation(#[from] DatasetValidationError),
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), PipelineRenderError> {
    let mut bytes = serde_json::to_vec(value).map_err(PipelineRenderError::SerializeMetadata)?;
    bytes.push(b'\n');
    let mut file = File::create(path).map_err(|source| PipelineRenderError::StagingIo {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(&bytes)
        .map_err(|source| PipelineRenderError::StagingIo {
            path: path.to_owned(),
            source,
        })
}

fn validate_report(
    report: &RenderReport,
    request: &RenderSceneRequest,
    backend: &bpp_core::BackendInfo,
    camera_count: u32,
) -> Result<(), PipelineRenderError> {
    let field = if report.scene_id() != request.scene().scene_id() {
        Some(RenderReportField::SceneId)
    } else if report.backend() != backend {
        Some(RenderReportField::Backend)
    } else if report.camera_count().get() != camera_count {
        Some(RenderReportField::CameraCount)
    } else if report.channels() != request.required_channels() {
        Some(RenderReportField::Channels)
    } else {
        None
    };
    match field {
        Some(field) => Err(PipelineRenderError::InvalidReport { field }),
        None => Ok(()),
    }
}

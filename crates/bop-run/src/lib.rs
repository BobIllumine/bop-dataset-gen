//! Run loading and end-to-end generation shared by the command line and the job server.
//!
//! This is the one place that names concrete renderer backends. [`generate`]
//! builds one renderer per configured render worker and picks the serial or
//! parallel pipeline, so the command line and the server always run a job the
//! same way. Backends are behind the `wgpu` and `blenderproc` features.
//!
//! Guide: <https://bobillumine.github.io/bop-dataset-gen/contributing/run-and-cli.html>

use std::path::{Path, PathBuf};

use bop_core::RunConfig;
use bop_pipeline::{
    CancellationToken, GenerationPipelineError, PreparedAssetCatalog, RunReport,
    SceneCommitObserver,
};
use thiserror::Error;

/// A parsed run configuration, the exact bytes it was parsed from, and the
/// directory its relative paths resolve against.
pub struct LoadedRun {
    bytes: Vec<u8>,
    config: RunConfig,
    directory: PathBuf,
}

impl LoadedRun {
    /// Parses resolved configuration bytes.
    ///
    /// `bytes` are stored verbatim: they are what the dataset manifest
    /// fingerprints and persists, so callers apply any rewrite first.
    ///
    /// # Errors
    ///
    /// Returns a message when the bytes are not UTF-8 or not a valid configuration.
    pub fn parse(bytes: Vec<u8>, directory: impl Into<PathBuf>) -> Result<Self, String> {
        let text =
            std::str::from_utf8(&bytes).map_err(|error| format!("config is not UTF-8: {error}"))?;
        let config = RunConfig::from_toml(text).map_err(|error| error.to_string())?;
        Ok(Self {
            bytes,
            config,
            directory: directory.into(),
        })
    }

    /// Returns the resolved configuration bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the validated configuration.
    #[must_use]
    pub const fn config(&self) -> &RunConfig {
        &self.config
    }

    /// Returns the directory relative configuration paths resolve against.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

/// A renderer backend a run can select.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RendererKind {
    /// The in-process wgpu rasterizer.
    Wgpu,
    /// The external `BlenderProc` worker.
    BlenderProc,
}

impl RendererKind {
    /// Returns the name the command line and the server API use.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Wgpu => "wgpu",
            Self::BlenderProc => "blenderproc",
        }
    }
}

/// `BlenderProc` paths that replace the configured ones when set.
///
/// The server sets both so a submitted configuration cannot choose which
/// programs it runs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BlenderProcRuntime {
    /// Replaces `[renderers.blenderproc] executable_path`.
    pub executable: Option<PathBuf>,
    /// Replaces `[renderers.blenderproc] worker_path`.
    pub worker: Option<PathBuf>,
}

/// Everything about a generation run that is not in its configuration.
#[derive(Clone, Default)]
pub struct GenerateOptions {
    /// Stops the run before its next scene.
    pub cancellation: CancellationToken,
    /// Called after each committed scene.
    pub observer: Option<SceneCommitObserver>,
    /// `BlenderProc` path overrides.
    pub blenderproc: BlenderProcRuntime,
}

/// A generation run that did not finish.
#[derive(Debug, Error)]
pub enum GenerateError {
    /// The selected renderer is not compiled in or could not start.
    #[error("{0}")]
    Unavailable(String),
    /// The pipeline could not be built from the configuration.
    #[error("generation setup failed: {0}")]
    Setup(GenerationPipelineError),
    /// A scene failed.
    #[error("generation failed: {0}")]
    Run(GenerationPipelineError),
}

impl GenerateError {
    /// Returns the command-line exit status for this failure.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Unavailable(_) => 3,
            Self::Setup(_)
            | Self::Run(
                GenerationPipelineError::InvalidExecutor(_)
                | GenerationPipelineError::RendererPool(_)
                | GenerationPipelineError::Request(_),
            ) => 2,
            Self::Run(
                GenerationPipelineError::Physics(_) | GenerationPipelineError::ParallelPhysics(_),
            ) => 4,
            Self::Run(
                GenerationPipelineError::Render(_) | GenerationPipelineError::RendererLifecycle(_),
            ) => 6,
            Self::Run(_) => 5,
        }
    }
}

/// Simulates, renders, and commits every pending scene of `loaded` into `output`.
///
/// Uses the parallel pipeline when more than one simulation or render worker
/// is configured. An existing compatible `output` is resumed.
///
/// # Errors
///
/// Returns why the renderer, pipeline, or a scene failed.
pub fn generate(
    loaded: &LoadedRun,
    prepared: &PreparedAssetCatalog,
    renderer: RendererKind,
    output: &Path,
    options: &GenerateOptions,
) -> Result<RunReport, GenerateError> {
    match renderer {
        RendererKind::Wgpu => generate_wgpu(loaded, prepared, output, options),
        RendererKind::BlenderProc => generate_blenderproc(loaded, prepared, output, options),
    }
}

#[cfg(feature = "wgpu")]
fn generate_wgpu(
    loaded: &LoadedRun,
    prepared: &PreparedAssetCatalog,
    output: &Path,
    options: &GenerateOptions,
) -> Result<RunReport, GenerateError> {
    let renderers = renderer_pool(loaded, |index, _| {
        bop_render_wgpu::WgpuRenderer::try_new_for_worker(
            loaded.config.plan().renderers().wgpu(),
            index,
        )
    })
    .map_err(|error| GenerateError::Unavailable(format!("wgpu unavailable: {error}")))?;
    run_generate(loaded, prepared, output, renderers, options)
}

#[cfg(not(feature = "wgpu"))]
fn generate_wgpu(
    _loaded: &LoadedRun,
    _prepared: &PreparedAssetCatalog,
    _output: &Path,
    _options: &GenerateOptions,
) -> Result<RunReport, GenerateError> {
    Err(GenerateError::Unavailable(
        "wgpu is unavailable in this build".to_owned(),
    ))
}

#[cfg(feature = "blenderproc")]
fn generate_blenderproc(
    loaded: &LoadedRun,
    prepared: &PreparedAssetCatalog,
    output: &Path,
    options: &GenerateOptions,
) -> Result<RunReport, GenerateError> {
    let config = resolved_blenderproc_config(loaded, &options.blenderproc);
    let renderers = renderer_pool(loaded, |index, count| {
        bop_render_blenderproc::BlenderProcRenderer::try_new_for_worker(
            config.clone(),
            index,
            count,
        )
    })
    .map_err(|error| GenerateError::Unavailable(format!("BlenderProc unavailable: {error}")))?;
    run_generate(loaded, prepared, output, renderers, options)
}

#[cfg(not(feature = "blenderproc"))]
fn generate_blenderproc(
    _loaded: &LoadedRun,
    _prepared: &PreparedAssetCatalog,
    _output: &Path,
    _options: &GenerateOptions,
) -> Result<RunReport, GenerateError> {
    Err(GenerateError::Unavailable(
        "BlenderProc is unavailable in this build".to_owned(),
    ))
}

#[cfg(any(feature = "wgpu", feature = "blenderproc"))]
fn run_generate<R>(
    loaded: &LoadedRun,
    prepared: &PreparedAssetCatalog,
    output: &Path,
    mut renderers: Vec<R>,
    options: &GenerateOptions,
) -> Result<RunReport, GenerateError>
where
    R: bop_render_api::Renderer,
{
    let plan = loaded.config.plan();
    let catalog = prepared.visual_assets();
    if plan.run().simulation_workers() > 1 || renderers.len() > 1 {
        let mut pipeline = bop_pipeline::ParallelGenerationPipeline::try_new(
            plan,
            prepared.shared_physics_assets(),
            renderers,
            output,
        )
        .map_err(GenerateError::Setup)?;
        if let Some(observer) = &options.observer {
            pipeline = pipeline.with_commit_observer(std::sync::Arc::clone(observer));
        }
        pipeline.run_until_cancelled(&loaded.bytes, catalog, &options.cancellation)
    } else {
        let renderer = renderers
            .pop()
            .expect("renderer_pool builds at least one renderer");
        let mut pipeline = bop_pipeline::SerialGenerationPipeline::try_new(
            plan,
            prepared.shared_physics_assets(),
            renderer,
            output,
        )
        .map_err(GenerateError::Setup)?;
        if let Some(observer) = &options.observer {
            pipeline = pipeline.with_commit_observer(std::sync::Arc::clone(observer));
        }
        pipeline.run_until_cancelled(&loaded.bytes, catalog, &options.cancellation)
    }
    .map_err(GenerateError::Run)
}

/// Builds one renderer per configured render worker; `make` receives the
/// worker's index and the worker count, so renderers can spread over GPUs.
///
/// # Errors
///
/// Returns the first renderer construction failure.
pub fn renderer_pool<R, E>(
    loaded: &LoadedRun,
    make: impl Fn(u32, u32) -> Result<R, E>,
) -> Result<Vec<R>, E> {
    let count = loaded.config.plan().run().renderer_workers().max(1);
    (0..count).map(|index| make(index, count)).collect()
}

/// Resolves the configured `BlenderProc` paths against the configuration directory,
/// replacing each with its `runtime` override when one is set.
///
/// # Panics
///
/// Never: the configured values were already checked.
#[cfg(feature = "blenderproc")]
#[must_use]
pub fn resolved_blenderproc_config(
    loaded: &LoadedRun,
    runtime: &BlenderProcRuntime,
) -> bop_core::BlenderProcRendererConfig {
    let configured = loaded.config.plan().renderers().blenderproc();
    let executable = runtime.executable.clone().unwrap_or_else(|| {
        resolve_executable_path(&loaded.directory, configured.executable_path())
    });
    let worker = runtime
        .worker
        .clone()
        .unwrap_or_else(|| resolve_runtime_path(&loaded.directory, configured.worker_path()));
    bop_core::BlenderProcRendererConfig::try_new(
        executable,
        worker,
        configured.device_selection(),
        configured.handshake_timeout_s(),
        configured.request_timeout_s(),
        configured.stderr_tail_capacity(),
    )
    .expect("resolving checked paths preserves renderer config invariants")
    .with_sampling(configured.sampling())
}

#[cfg(feature = "blenderproc")]
fn resolve_runtime_path(base: &Path, configured: &Path) -> PathBuf {
    if configured.is_absolute() {
        configured.to_owned()
    } else {
        base.join(configured)
    }
}

/// A bare program name is looked up on `PATH`; any other relative path is
/// resolved against the configuration directory.
#[cfg(feature = "blenderproc")]
fn resolve_executable_path(base: &Path, configured: &Path) -> PathBuf {
    if configured.is_absolute() || configured.components().count() == 1 {
        configured.to_owned()
    } else {
        base.join(configured)
    }
}

#[cfg(all(test, feature = "blenderproc"))]
mod tests {
    use std::path::{Path, PathBuf};

    #[test]
    fn blenderproc_bare_executable_uses_path_but_relative_worker_paths_use_config_directory() {
        let base = Path::new("/project/config");
        assert_eq!(
            super::resolve_executable_path(base, Path::new("blenderproc")),
            PathBuf::from("blenderproc")
        );
        assert_eq!(
            super::resolve_executable_path(base, Path::new("tools/blenderproc")),
            PathBuf::from("/project/config/tools/blenderproc")
        );
    }
}

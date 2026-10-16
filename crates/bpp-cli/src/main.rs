//! Command-line entry point for BPP dataset generation.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::ExitCode,
};

#[cfg(feature = "blenderproc")]
use bpp_core::Renderer as _;

fn main() -> ExitCode {
    run(std::env::args().skip(1))
}

fn run(arguments: impl IntoIterator<Item = String>) -> ExitCode {
    match parse(arguments) {
        Ok(command) => execute(command),
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

const USAGE: &str = "usage: bpp doctor [--renderer wgpu|blenderproc|all]\n       bpp schema\n       bpp prepare-assets --config FILE\n       bpp simulate --config FILE --output CORPUS\n       bpp render --config FILE --snapshots CORPUS --renderer BACKEND --output DATASET\n       bpp generate --config FILE --renderer BACKEND --output DATASET [--resume]\n       bpp validate DATASET\n       bpp export bop DATASET --output DIRECTORY\n       bpp bench --config FILE --renderers BACKEND[,BACKEND] --output DIRECTORY";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RendererSelection {
    Wgpu,
    BlenderProc,
    All,
}

impl RendererSelection {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "wgpu" => Ok(Self::Wgpu),
            "blenderproc" => Ok(Self::BlenderProc),
            "all" => Ok(Self::All),
            _ => Err(format!("unknown renderer {value:?}")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Doctor {
        renderer: Option<RendererSelection>,
    },
    Schema,
    PrepareAssets {
        config: PathBuf,
    },
    Simulate {
        config: PathBuf,
        output: PathBuf,
    },
    Render {
        config: PathBuf,
        snapshots: PathBuf,
        renderer: RendererSelection,
        output: PathBuf,
    },
    Generate {
        config: PathBuf,
        renderer: RendererSelection,
        output: PathBuf,
        resume: bool,
    },
    Validate {
        dataset: PathBuf,
    },
    ExportBop {
        dataset: PathBuf,
        output: PathBuf,
    },
    Bench {
        config: PathBuf,
        renderers: Vec<RendererSelection>,
        output: PathBuf,
    },
}

fn parse(arguments: impl IntoIterator<Item = impl Into<String>>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter().map(Into::into);
    let command = arguments
        .next()
        .ok_or_else(|| "missing subcommand".to_owned())?;
    let rest = arguments.collect::<Vec<_>>();
    match command.as_str() {
        "doctor" => {
            let options = parse_options(&rest, &["--renderer"], &[])?;
            ensure_no_flags(&options.flags)?;
            let renderer = options
                .values
                .get("--renderer")
                .map(|value| RendererSelection::parse(value))
                .transpose()?;
            Ok(Command::Doctor { renderer })
        }
        "schema" if rest.is_empty() => Ok(Command::Schema),
        "prepare-assets" => {
            let options = parse_options(&rest, &["--config"], &[])?;
            ensure_no_flags(&options.flags)?;
            Ok(Command::PrepareAssets {
                config: required_path(&options.values, "--config")?,
            })
        }
        "simulate" => {
            let options = parse_options(&rest, &["--config", "--output"], &[])?;
            ensure_no_flags(&options.flags)?;
            Ok(Command::Simulate {
                config: required_path(&options.values, "--config")?,
                output: required_path(&options.values, "--output")?,
            })
        }
        "render" => {
            let options = parse_options(
                &rest,
                &["--config", "--snapshots", "--renderer", "--output"],
                &[],
            )?;
            ensure_no_flags(&options.flags)?;
            Ok(Command::Render {
                config: required_path(&options.values, "--config")?,
                snapshots: required_path(&options.values, "--snapshots")?,
                renderer: one_renderer(required(&options.values, "--renderer")?)?,
                output: required_path(&options.values, "--output")?,
            })
        }
        "generate" => {
            let options = parse_options(
                &rest,
                &["--config", "--renderer", "--output"],
                &["--resume"],
            )?;
            Ok(Command::Generate {
                config: required_path(&options.values, "--config")?,
                renderer: one_renderer(required(&options.values, "--renderer")?)?,
                output: required_path(&options.values, "--output")?,
                resume: options.flags.contains("--resume"),
            })
        }
        "validate" if rest.len() == 1 => Ok(Command::Validate {
            dataset: PathBuf::from(&rest[0]),
        }),
        "export" if rest.first().map(String::as_str) == Some("bop") => {
            let dataset = rest
                .get(1)
                .filter(|value| !value.starts_with('-'))
                .ok_or_else(|| "export bop requires DATASET".to_owned())?;
            let options = parse_options(&rest[2..], &["--output"], &[])?;
            ensure_no_flags(&options.flags)?;
            Ok(Command::ExportBop {
                dataset: PathBuf::from(dataset),
                output: required_path(&options.values, "--output")?,
            })
        }
        "bench" => {
            let options = parse_options(&rest, &["--config", "--renderers", "--output"], &[])?;
            ensure_no_flags(&options.flags)?;
            let renderers = required(&options.values, "--renderers")?
                .split(',')
                .map(one_renderer)
                .collect::<Result<Vec<_>, _>>()?;
            if renderers.is_empty() {
                return Err("--renderers must not be empty".to_owned());
            }
            Ok(Command::Bench {
                config: required_path(&options.values, "--config")?,
                renderers,
                output: required_path(&options.values, "--output")?,
            })
        }
        _ => Err(format!("unknown or malformed subcommand {command:?}")),
    }
}

struct ParsedOptions {
    values: BTreeMap<String, String>,
    flags: BTreeSet<String>,
}

fn parse_options(
    arguments: &[String],
    value_options: &[&str],
    boolean_flags: &[&str],
) -> Result<ParsedOptions, String> {
    let mut options = BTreeMap::new();
    let mut flags = BTreeSet::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if boolean_flags.contains(&argument) {
            if !flags.insert(argument.to_owned()) {
                return Err(format!("duplicate flag {argument}"));
            }
            index += 1;
        } else if value_options.contains(&argument) {
            let value = arguments
                .get(index + 1)
                .filter(|value| !value.starts_with('-'))
                .ok_or_else(|| format!("{argument} requires a value"))?;
            if options.insert(argument.to_owned(), value.clone()).is_some() {
                return Err(format!("duplicate option {argument}"));
            }
            index += 2;
        } else {
            return Err(format!("unexpected argument {argument:?}"));
        }
    }
    Ok(ParsedOptions {
        values: options,
        flags,
    })
}

fn required<'a>(options: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, String> {
    options
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("missing required option {name}"))
}

fn required_path(options: &BTreeMap<String, String>, name: &str) -> Result<PathBuf, String> {
    required(options, name).map(PathBuf::from)
}

fn ensure_no_flags(flags: &BTreeSet<String>) -> Result<(), String> {
    if flags.is_empty() {
        Ok(())
    } else {
        Err("unexpected boolean flag".to_owned())
    }
}

fn one_renderer(value: &str) -> Result<RendererSelection, String> {
    match RendererSelection::parse(value)? {
        RendererSelection::All => Err("renderer 'all' is only valid for doctor".to_owned()),
        renderer => Ok(renderer),
    }
}

fn execute(command: Command) -> ExitCode {
    match command {
        Command::Doctor {
            renderer: None | Some(RendererSelection::All),
        } => {
            let wgpu = doctor("wgpu");
            let blenderproc = doctor("blenderproc");
            if wgpu == ExitCode::SUCCESS && blenderproc == ExitCode::SUCCESS {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            }
        }
        Command::Doctor {
            renderer: Some(RendererSelection::Wgpu),
        } => doctor("wgpu"),
        Command::Doctor {
            renderer: Some(RendererSelection::BlenderProc),
        } => doctor("blenderproc"),
        Command::Schema => {
            println!("{}", bpp_core::run_config_json_schema());
            ExitCode::SUCCESS
        }
        Command::PrepareAssets { config } => prepare_assets(&config),
        Command::Simulate { config, output } => simulate(&config, &output),
        Command::Render {
            config,
            snapshots,
            renderer,
            output,
        } => render(&config, &snapshots, renderer, &output),
        Command::Generate {
            config,
            renderer,
            output,
            resume,
        } => generate(&config, renderer, &output, resume),
        Command::Validate { dataset } => validate(&dataset),
        other => unavailable_command(&other),
    }
}

fn validate(dataset: &std::path::Path) -> ExitCode {
    match bpp_dataset::validate_dataset(dataset) {
        Ok(report) => {
            println!(
                "{}",
                serde_json::json!({
                    "scene_count": report.scene_count(),
                    "file_count": report.file_count(),
                })
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("dataset validation failed: {error}");
            ExitCode::from(5)
        }
    }
}

fn prepare_assets(config_path: &std::path::Path) -> ExitCode {
    let loaded = match load_run(config_path) {
        Ok(loaded) => loaded,
        Err(error) => return configuration_error(&error),
    };
    match bpp_pipeline::prepare_asset_catalog(loaded.config.plan(), &loaded.directory) {
        Ok(prepared) => {
            println!(
                "{}",
                serde_json::json!({
                    "asset_count": prepared.visual_assets().len(),
                    "asset_catalog_fingerprint": prepared.visual_assets().fingerprint(),
                })
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("asset preparation failed: {error}");
            ExitCode::from(2)
        }
    }
}

struct LoadedRun {
    bytes: Vec<u8>,
    config: bpp_core::RunConfig,
    directory: PathBuf,
}

fn load_run(config_path: &std::path::Path) -> Result<LoadedRun, String> {
    let bytes = std::fs::read(config_path)
        .map_err(|error| format!("cannot read config {}: {error}", config_path.display()))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| format!("config {} is not UTF-8: {error}", config_path.display()))?;
    let config = bpp_core::RunConfig::from_toml(text)
        .map_err(|error| format!("invalid config {}: {error}", config_path.display()))?;
    let directory = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_owned();
    Ok(LoadedRun {
        bytes,
        config,
        directory,
    })
}

fn configuration_error(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
}

fn simulate(config_path: &std::path::Path, output: &std::path::Path) -> ExitCode {
    let loaded = match load_run(config_path) {
        Ok(loaded) => loaded,
        Err(error) => return configuration_error(&error),
    };
    let prepared =
        match bpp_pipeline::prepare_asset_catalog(loaded.config.plan(), &loaded.directory) {
            Ok(prepared) => prepared,
            Err(error) => {
                return configuration_error(&format!("asset preparation failed: {error}"));
            }
        };
    let simulator = match bpp_pipeline::SerialSnapshotSimulator::try_new(
        loaded.config.plan(),
        prepared.shared_physics_assets(),
    ) {
        Ok(simulator) => simulator,
        Err(error) => return configuration_error(&format!("simulation setup failed: {error}")),
    };
    let cancellation = bpp_pipeline::CancellationToken::new();
    match simulator.run(
        &loaded.bytes,
        prepared.visual_assets().fingerprint(),
        output,
        &cancellation,
    ) {
        Ok(report) => {
            println!(
                "{}",
                serde_json::json!({
                    "preexisting_scenes": report.preexisting_scene_count(),
                    "generated_scenes": report.generated_scene_count(),
                    "total_scenes": report.total_scene_count(),
                    "cancelled": report.was_cancelled(),
                })
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("simulation did not produce the requested corpus: {error}");
            ExitCode::from(4)
        }
    }
}

fn render(
    config_path: &std::path::Path,
    snapshots: &std::path::Path,
    renderer: RendererSelection,
    output: &std::path::Path,
) -> ExitCode {
    let loaded = match load_run(config_path) {
        Ok(loaded) => loaded,
        Err(error) => return configuration_error(&error),
    };
    let prepared =
        match bpp_pipeline::prepare_asset_catalog(loaded.config.plan(), &loaded.directory) {
            Ok(prepared) => prepared,
            Err(error) => {
                return configuration_error(&format!("asset preparation failed: {error}"));
            }
        };
    match renderer {
        RendererSelection::Wgpu => render_wgpu(&loaded, &prepared, snapshots, output),
        RendererSelection::BlenderProc => render_blenderproc(&loaded, &prepared, snapshots, output),
        RendererSelection::All => unreachable!("parser rejects all for render"),
    }
}

fn generate(
    config_path: &std::path::Path,
    renderer: RendererSelection,
    output: &std::path::Path,
    resume: bool,
) -> ExitCode {
    let loaded = match load_run(config_path) {
        Ok(loaded) => loaded,
        Err(error) => return configuration_error(&error),
    };
    if !resume && output.exists() {
        return configuration_error(&format!(
            "output {} already exists; pass --resume to continue it",
            output.display()
        ));
    }
    let prepared =
        match bpp_pipeline::prepare_asset_catalog(loaded.config.plan(), &loaded.directory) {
            Ok(prepared) => prepared,
            Err(error) => {
                return configuration_error(&format!("asset preparation failed: {error}"));
            }
        };
    match renderer {
        RendererSelection::Wgpu => generate_wgpu(&loaded, &prepared, output),
        RendererSelection::BlenderProc => generate_blenderproc(&loaded, &prepared, output),
        RendererSelection::All => unreachable!("parser rejects all for generate"),
    }
}

#[cfg(feature = "wgpu")]
fn render_wgpu(
    loaded: &LoadedRun,
    prepared: &bpp_pipeline::PreparedAssetCatalog,
    snapshots: &std::path::Path,
    output: &std::path::Path,
) -> ExitCode {
    match bpp_render_wgpu::WgpuRenderer::try_new(loaded.config.plan().renderers().wgpu()) {
        Ok(renderer) => run_render(loaded, prepared, snapshots, output, renderer),
        Err(error) => unavailable(format!("wgpu unavailable: {error}")),
    }
}

#[cfg(not(feature = "wgpu"))]
fn render_wgpu(
    _loaded: &LoadedRun,
    _prepared: &bpp_pipeline::PreparedAssetCatalog,
    _snapshots: &std::path::Path,
    _output: &std::path::Path,
) -> ExitCode {
    unavailable("wgpu is unavailable in this build")
}

#[cfg(feature = "blenderproc")]
fn render_blenderproc(
    loaded: &LoadedRun,
    prepared: &bpp_pipeline::PreparedAssetCatalog,
    snapshots: &std::path::Path,
    output: &std::path::Path,
) -> ExitCode {
    let config = resolved_blenderproc_config(loaded);
    match bpp_render_blenderproc::BlenderProcRenderer::try_new(config) {
        Ok(renderer) => run_render(loaded, prepared, snapshots, output, renderer),
        Err(error) => unavailable(format!("BlenderProc unavailable: {error}")),
    }
}

#[cfg(not(feature = "blenderproc"))]
fn render_blenderproc(
    _loaded: &LoadedRun,
    _prepared: &bpp_pipeline::PreparedAssetCatalog,
    _snapshots: &std::path::Path,
    _output: &std::path::Path,
) -> ExitCode {
    unavailable("BlenderProc is unavailable in this build")
}

fn run_render<R>(
    loaded: &LoadedRun,
    prepared: &bpp_pipeline::PreparedAssetCatalog,
    snapshots: &std::path::Path,
    output: &std::path::Path,
    renderer: R,
) -> ExitCode
where
    R: bpp_core::Renderer<
            AssetCatalog = bpp_assets::ResolvedAssetCatalog,
            StagingArea = bpp_dataset::SceneStagingArea,
        >,
{
    let mut pipeline =
        bpp_pipeline::SerialSnapshotRenderer::new(loaded.config.plan(), renderer, output);
    match pipeline.run_to_completion(&loaded.bytes, prepared.visual_assets(), snapshots) {
        Ok(report) => print_run_report(report),
        Err(error) => {
            eprintln!("render failed: {error}");
            match error {
                bpp_pipeline::SerialSnapshotRenderError::Render(_)
                | bpp_pipeline::SerialSnapshotRenderError::RendererLifecycle(_) => {
                    ExitCode::from(6)
                }
                _ => ExitCode::from(5),
            }
        }
    }
}

#[cfg(feature = "wgpu")]
fn generate_wgpu(
    loaded: &LoadedRun,
    prepared: &bpp_pipeline::PreparedAssetCatalog,
    output: &std::path::Path,
) -> ExitCode {
    match bpp_render_wgpu::WgpuRenderer::try_new(loaded.config.plan().renderers().wgpu()) {
        Ok(renderer) => run_generate(loaded, prepared, output, renderer),
        Err(error) => unavailable(format!("wgpu unavailable: {error}")),
    }
}

#[cfg(not(feature = "wgpu"))]
fn generate_wgpu(
    _loaded: &LoadedRun,
    _prepared: &bpp_pipeline::PreparedAssetCatalog,
    _output: &std::path::Path,
) -> ExitCode {
    unavailable("wgpu is unavailable in this build")
}

#[cfg(feature = "blenderproc")]
fn generate_blenderproc(
    loaded: &LoadedRun,
    prepared: &bpp_pipeline::PreparedAssetCatalog,
    output: &std::path::Path,
) -> ExitCode {
    match bpp_render_blenderproc::BlenderProcRenderer::try_new(resolved_blenderproc_config(loaded))
    {
        Ok(renderer) => run_generate(loaded, prepared, output, renderer),
        Err(error) => unavailable(format!("BlenderProc unavailable: {error}")),
    }
}

#[cfg(not(feature = "blenderproc"))]
fn generate_blenderproc(
    _loaded: &LoadedRun,
    _prepared: &bpp_pipeline::PreparedAssetCatalog,
    _output: &std::path::Path,
) -> ExitCode {
    unavailable("BlenderProc is unavailable in this build")
}

fn run_generate<R>(
    loaded: &LoadedRun,
    prepared: &bpp_pipeline::PreparedAssetCatalog,
    output: &std::path::Path,
    renderer: R,
) -> ExitCode
where
    R: bpp_core::Renderer<
            AssetCatalog = bpp_assets::ResolvedAssetCatalog,
            StagingArea = bpp_dataset::SceneStagingArea,
        >,
{
    let mut pipeline = match bpp_pipeline::SerialGenerationPipeline::try_new(
        loaded.config.plan(),
        prepared.shared_physics_assets(),
        renderer,
        output,
    ) {
        Ok(pipeline) => pipeline,
        Err(error) => return configuration_error(&format!("generation setup failed: {error}")),
    };
    match pipeline.run_to_completion(&loaded.bytes, prepared.visual_assets()) {
        Ok(report) => print_run_report(report),
        Err(error) => {
            eprintln!("generation failed: {error}");
            match error {
                bpp_pipeline::SerialGenerationPipelineError::Physics(_) => ExitCode::from(4),
                bpp_pipeline::SerialGenerationPipelineError::Render(_)
                | bpp_pipeline::SerialGenerationPipelineError::RendererLifecycle(_) => {
                    ExitCode::from(6)
                }
                bpp_pipeline::SerialGenerationPipelineError::Request(_) => ExitCode::from(2),
                _ => ExitCode::from(5),
            }
        }
    }
}

fn print_run_report(report: bpp_pipeline::SerialRunReport) -> ExitCode {
    println!(
        "{}",
        serde_json::json!({
            "preexisting_scenes": report.preexisting_scene_count(),
            "generated_scenes": report.generated_scene_count(),
            "total_scenes": report.total_scene_count(),
            "cancelled": report.was_cancelled(),
        })
    );
    ExitCode::SUCCESS
}

fn unavailable(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(3)
}

#[cfg(feature = "blenderproc")]
fn resolved_blenderproc_config(loaded: &LoadedRun) -> bpp_core::BlenderProcRendererConfig {
    let configured = loaded.config.plan().renderers().blenderproc();
    let executable = std::env::var_os("BPP_BLENDERPROC_EXECUTABLE").map_or_else(
        || resolve_executable_path(&loaded.directory, configured.executable_path()),
        PathBuf::from,
    );
    let worker = resolve_runtime_path(&loaded.directory, configured.worker_path());
    bpp_core::BlenderProcRendererConfig::try_new(
        executable,
        worker,
        configured.device_selection(),
        configured.handshake_timeout_s(),
        configured.request_timeout_s(),
        configured.stderr_tail_capacity(),
    )
    .expect("resolving checked paths preserves renderer config invariants")
}

#[cfg(feature = "blenderproc")]
fn resolve_runtime_path(base: &std::path::Path, configured: &std::path::Path) -> PathBuf {
    if configured.is_absolute() {
        configured.to_owned()
    } else {
        base.join(configured)
    }
}

#[cfg(feature = "blenderproc")]
fn resolve_executable_path(base: &std::path::Path, configured: &std::path::Path) -> PathBuf {
    if configured.is_absolute() || configured.components().count() == 1 {
        configured.to_owned()
    } else {
        base.join(configured)
    }
}

fn unavailable_command(command: &Command) -> ExitCode {
    eprintln!(
        "subcommand {} is recognized but its runtime is not available yet",
        match command {
            Command::PrepareAssets { .. } => "prepare-assets",
            Command::Simulate { .. } => "simulate",
            Command::Render { .. } => "render",
            Command::Generate { .. } => "generate",
            Command::ExportBop { .. } => "export bop",
            Command::Bench { .. } => "bench",
            Command::Doctor { .. } | Command::Schema | Command::Validate { .. } => {
                unreachable!("implemented command")
            }
        }
    );
    ExitCode::from(3)
}

fn doctor(renderer: &str) -> ExitCode {
    match renderer {
        "wgpu" => doctor_wgpu(),
        "blenderproc" => doctor_blenderproc(),
        _ => {
            eprintln!("renderer {renderer:?} is unavailable in this build");
            ExitCode::from(3)
        }
    }
}

#[cfg(feature = "wgpu")]
fn doctor_wgpu() -> ExitCode {
    let config = bpp_core::WgpuRendererConfig::try_new(
        bpp_core::WgpuDevicePreference::Automatic,
        bpp_core::WgpuAdapterPreference::HighPerformance,
        2048,
        false,
    )
    .expect("fixed doctor configuration");
    match bpp_render_wgpu::WgpuRenderer::try_new(config) {
        Ok(renderer) => {
            let value = serde_json::json!({
                "renderer": "wgpu",
                "available": true,
                "backend": renderer.info(),
                "capabilities": renderer.capabilities(),
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).expect("report serialization")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("wgpu unavailable: {error}");
            ExitCode::from(3)
        }
    }
}

#[cfg(not(feature = "wgpu"))]
fn doctor_wgpu() -> ExitCode {
    eprintln!("renderer \"wgpu\" is unavailable in this build");
    ExitCode::from(3)
}

#[cfg(feature = "blenderproc")]
fn doctor_blenderproc() -> ExitCode {
    let executable =
        std::env::var_os("BPP_BLENDERPROC_EXECUTABLE").unwrap_or_else(|| "blenderproc".into());
    let worker = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/blenderproc-worker/blenderproc_worker.py");
    let config = bpp_core::BlenderProcRendererConfig::try_new(
        executable, worker, "AUTO", 30.0, 300.0, 65_536,
    )
    .expect("fixed doctor configuration");
    match bpp_render_blenderproc::BlenderProcRenderer::try_new(config) {
        Ok(renderer) => {
            let value = serde_json::json!({
                "renderer": "blenderproc",
                "available": true,
                "backend": renderer.info(),
                "capabilities": renderer.capabilities(),
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).expect("report serialization")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("BlenderProc unavailable: {error}");
            ExitCode::from(3)
        }
    }
}

#[cfg(not(feature = "blenderproc"))]
fn doctor_blenderproc() -> ExitCode {
    eprintln!("renderer \"blenderproc\" is unavailable in this build");
    ExitCode::from(3)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "wgpu")]
    use std::fs;
    use std::path::PathBuf;

    use super::{Command, RendererSelection, parse, run};

    #[test]
    fn every_documented_subcommand_has_one_unambiguous_parse_shape() {
        assert_eq!(parse(["doctor"]), Ok(Command::Doctor { renderer: None }));
        assert_eq!(
            parse(["doctor", "--renderer", "all"]),
            Ok(Command::Doctor {
                renderer: Some(RendererSelection::All)
            })
        );
        assert_eq!(parse(["schema"]), Ok(Command::Schema));
        assert_eq!(
            parse(["prepare-assets", "--config", "run.toml"]),
            Ok(Command::PrepareAssets {
                config: PathBuf::from("run.toml")
            })
        );
        assert_eq!(
            parse(["simulate", "--config", "run.toml", "--output", "corpus"]),
            Ok(Command::Simulate {
                config: PathBuf::from("run.toml"),
                output: PathBuf::from("corpus")
            })
        );
        assert_eq!(
            parse([
                "render",
                "--config",
                "run.toml",
                "--snapshots",
                "corpus",
                "--renderer",
                "wgpu",
                "--output",
                "dataset"
            ]),
            Ok(Command::Render {
                config: PathBuf::from("run.toml"),
                snapshots: PathBuf::from("corpus"),
                renderer: RendererSelection::Wgpu,
                output: PathBuf::from("dataset")
            })
        );
        assert_eq!(
            parse([
                "generate",
                "--config",
                "run.toml",
                "--renderer",
                "blenderproc",
                "--output",
                "dataset",
                "--resume"
            ]),
            Ok(Command::Generate {
                config: PathBuf::from("run.toml"),
                renderer: RendererSelection::BlenderProc,
                output: PathBuf::from("dataset"),
                resume: true
            })
        );
        assert_eq!(
            parse(["validate", "dataset"]),
            Ok(Command::Validate {
                dataset: PathBuf::from("dataset")
            })
        );
        assert_eq!(
            parse(["export", "bop", "dataset", "--output", "bop-out"]),
            Ok(Command::ExportBop {
                dataset: PathBuf::from("dataset"),
                output: PathBuf::from("bop-out")
            })
        );
        assert_eq!(
            parse([
                "bench",
                "--config",
                "run.toml",
                "--renderers",
                "wgpu,blenderproc",
                "--output",
                "reports"
            ]),
            Ok(Command::Bench {
                config: PathBuf::from("run.toml"),
                renderers: vec![RendererSelection::Wgpu, RendererSelection::BlenderProc],
                output: PathBuf::from("reports")
            })
        );
    }

    #[test]
    fn parser_rejects_missing_duplicate_unknown_and_ambiguous_options() {
        for arguments in [
            vec!["simulate", "--config", "run.toml"],
            vec![
                "generate",
                "--config",
                "a.toml",
                "--config",
                "b.toml",
                "--renderer",
                "wgpu",
                "--output",
                "out",
            ],
            vec![
                "render",
                "--config",
                "run.toml",
                "--snapshots",
                "corpus",
                "--renderer",
                "all",
                "--output",
                "out",
            ],
            vec!["validate", "one", "two"],
            vec!["schema", "--output", "schema.json"],
        ] {
            assert!(parse(arguments).is_err());
        }
    }

    #[test]
    fn invalid_doctor_invocations_use_the_usage_exit_code() {
        assert_eq!(run([]), std::process::ExitCode::from(2));
        assert_eq!(
            run(["doctor".to_owned(), "--wat".to_owned()]),
            std::process::ExitCode::from(2)
        );
    }

    #[test]
    fn invalid_renderer_is_usage_and_uncompiled_renderer_is_capability_failure() {
        assert_eq!(
            run([
                "doctor".to_owned(),
                "--renderer".to_owned(),
                "unknown".to_owned()
            ]),
            std::process::ExitCode::from(2)
        );
        assert_eq!(
            run([
                "doctor".to_owned(),
                "--renderer".to_owned(),
                "blenderproc".to_owned()
            ]),
            std::process::ExitCode::from(3)
        );
    }

    #[test]
    fn prepare_assets_reports_missing_config_as_configuration_failure() {
        let missing = std::env::temp_dir().join(format!(
            "bpp-missing-config-{}-{}.toml",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        assert_eq!(
            run([
                "prepare-assets".to_owned(),
                "--config".to_owned(),
                missing.display().to_string(),
            ]),
            std::process::ExitCode::from(2)
        );
    }

    #[test]
    fn simulate_reports_missing_config_as_configuration_failure() {
        let missing = std::env::temp_dir().join(format!(
            "bpp-missing-simulation-config-{}.toml",
            std::process::id()
        ));
        assert_eq!(
            run([
                "simulate".to_owned(),
                "--config".to_owned(),
                missing.display().to_string(),
                "--output".to_owned(),
                "unused-corpus".to_owned(),
            ]),
            std::process::ExitCode::from(2)
        );
    }

    #[test]
    fn render_and_generate_report_missing_configs_as_configuration_failures() {
        let missing = std::env::temp_dir().join(format!(
            "bpp-missing-render-config-{}.toml",
            std::process::id()
        ));
        let path = missing.display().to_string();
        assert_eq!(
            run([
                "render".to_owned(),
                "--config".to_owned(),
                path.clone(),
                "--snapshots".to_owned(),
                "unused-corpus".to_owned(),
                "--renderer".to_owned(),
                "wgpu".to_owned(),
                "--output".to_owned(),
                "unused-dataset".to_owned(),
            ]),
            std::process::ExitCode::from(2)
        );
        assert_eq!(
            run([
                "generate".to_owned(),
                "--config".to_owned(),
                path,
                "--renderer".to_owned(),
                "wgpu".to_owned(),
                "--output".to_owned(),
                "unused-dataset".to_owned(),
            ]),
            std::process::ExitCode::from(2)
        );
    }

    #[cfg(feature = "blenderproc")]
    #[test]
    fn blenderproc_bare_executable_uses_path_but_relative_worker_paths_use_config_directory() {
        let base = std::path::Path::new("/project/config");
        assert_eq!(
            super::resolve_executable_path(base, std::path::Path::new("blenderproc")),
            PathBuf::from("blenderproc")
        );
        assert_eq!(
            super::resolve_executable_path(base, std::path::Path::new("tools/blenderproc")),
            PathBuf::from("/project/config/tools/blenderproc")
        );
    }

    #[cfg(feature = "wgpu")]
    #[test]
    #[ignore = "requires access to a host Metal or Vulkan adapter"]
    fn real_wgpu_generate_cli_commits_a_complete_dataset() {
        let root = tempfile::tempdir().expect("temporary CLI smoke root");
        let source = root.path().join("cube.stl");
        fs::write(
            &source,
            "solid tetrahedron\n\
             facet normal 0 0 -1\nouter loop\nvertex -1 -1 0\nvertex 0 1 0\nvertex 1 -1 0\nendloop\nendfacet\n\
             facet normal 0 -1 0\nouter loop\nvertex -1 -1 0\nvertex 1 -1 0\nvertex 0 0 1\nendloop\nendfacet\n\
             facet normal 1 1 1\nouter loop\nvertex 1 -1 0\nvertex 0 1 0\nvertex 0 0 1\nendloop\nendfacet\n\
             facet normal -1 1 1\nouter loop\nvertex 0 1 0\nvertex -1 -1 0\nvertex 0 0 1\nendloop\nendfacet\n\
             endsolid tetrahedron\n",
        )
        .expect("write generated permissive STL");
        let cache = root.path().join("cache");
        let config = include_str!("../../bpp-pipeline/tests/fixtures/serial_run.toml")
            .replace("source_path = \"cube.glb\"", "source_path = \"cube.stl\"")
            .replace("source_unit_scale_m = 1.0", "source_unit_scale_m = 0.05")
            .replace(
                "cache_path = \"cache\"",
                &format!("cache_path = \"{}\"", cache.display()),
            );
        let config_path = root.path().join("run.toml");
        fs::write(&config_path, config).expect("write smoke config");
        let dataset = root.path().join("dataset");

        let exit = run([
            "generate".to_owned(),
            "--config".to_owned(),
            config_path.display().to_string(),
            "--renderer".to_owned(),
            "wgpu".to_owned(),
            "--output".to_owned(),
            dataset.display().to_string(),
        ]);

        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        let frame = dataset.join("scenes/00000000000000000000/frames/00001");
        for artifact in ["frame.json", "rgb.png", "depth.exr", "instance.png"] {
            let path = frame.join(artifact);
            assert!(path.metadata().is_ok_and(|metadata| metadata.len() > 0));
        }

        assert_eq!(
            run(["validate".to_owned(), dataset.display().to_string()]),
            std::process::ExitCode::SUCCESS
        );
        fs::write(frame.join("rgb.png"), b"corrupt").expect("corrupt committed RGB");
        assert_eq!(
            run(["validate".to_owned(), dataset.display().to_string()]),
            std::process::ExitCode::from(5)
        );
    }
}

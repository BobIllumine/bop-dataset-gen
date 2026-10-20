//! Command-line entry point for BOP dataset generation.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::ExitCode,
};

#[cfg(feature = "blenderproc")]
use bop_render_api::Renderer as _;

mod bench;
mod interrupt;

use bop_run::{LoadedRun, renderer_pool};

fn main() -> ExitCode {
    run(std::env::args().skip(1))
}

fn run(arguments: impl IntoIterator<Item = String>) -> ExitCode {
    let mut arguments = arguments.into_iter().collect::<Vec<_>>();
    let log_format = match take_log_format(&mut arguments) {
        Ok(format) => format,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    init_logging(log_format);
    match parse(arguments) {
        Ok(command) => execute(command),
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogFormat {
    Human,
    Json,
}

/// Removes a leading `--log-format human|json` option.
fn take_log_format(arguments: &mut Vec<String>) -> Result<LogFormat, String> {
    if arguments.first().map(String::as_str) != Some("--log-format") {
        return Ok(LogFormat::Human);
    }
    let format = match arguments.get(1).map(String::as_str) {
        Some("human") => LogFormat::Human,
        Some("json") => LogFormat::Json,
        other => return Err(format!("--log-format must be human or json, not {other:?}")),
    };
    arguments.drain(..2);
    Ok(format)
}

/// Sends logs to stderr, filtered by `BOP_LOG` (default `info`).
///
/// Stdout stays reserved for each command's machine-readable JSON result.
fn init_logging(format: LogFormat) {
    let filter = tracing_subscriber::EnvFilter::try_from_env("BOP_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false);
    // Tests call `run` repeatedly in one process; only the first install wins.
    let _ = match format {
        LogFormat::Human => builder.without_time().try_init(),
        LogFormat::Json => builder.json().try_init(),
    };
}

/// Run-identity values the command line may override; each is written into
/// the resolved configuration so provenance and resume see the real values.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Overrides {
    seed: Option<u64>,
    scenes: Option<u32>,
}

impl Overrides {
    fn parse(values: &BTreeMap<String, String>) -> Result<Self, String> {
        let number = |flag: &str| {
            values
                .get(flag)
                .map(|value| {
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("{flag} must be a non-negative integer"))
                })
                .transpose()
        };
        let scenes = number("--scenes")?
            .map(|value| u32::try_from(value).map_err(|_| "--scenes is too large".to_owned()))
            .transpose()?;
        Ok(Self {
            seed: number("--seed")?,
            scenes,
        })
    }

    const fn is_empty(self) -> bool {
        self.seed.is_none() && self.scenes.is_none()
    }

    /// Rewrites `[run]` in the configuration text with the overridden values.
    fn apply(self, text: &str) -> Result<String, String> {
        if self.is_empty() {
            return Ok(text.to_owned());
        }
        let mut table = text
            .parse::<toml::Table>()
            .map_err(|error| format!("config is not valid TOML: {error}"))?;
        let run = table
            .get_mut("run")
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| "config has no [run] table to override".to_owned())?;
        if let Some(seed) = self.seed {
            let seed = i64::try_from(seed)
                .map_err(|_| "--seed must fit in a signed 64-bit TOML integer".to_owned())?;
            run.insert("master_seed".to_owned(), toml::Value::Integer(seed));
        }
        if let Some(scenes) = self.scenes {
            run.insert(
                "valid_scene_count".to_owned(),
                toml::Value::Integer(i64::from(scenes)),
            );
        }
        toml::to_string(&table).map_err(|error| format!("cannot write resolved config: {error}"))
    }
}

const USAGE: &str = "usage: bop-gen doctor [--renderer wgpu|blenderproc|all]\n       bop-gen schema\n       bop-gen prepare-assets --config FILE\n       bop-gen simulate --config FILE --output CORPUS [OVERRIDES]\n       bop-gen render --config FILE --snapshots CORPUS --renderer BACKEND --output DATASET [OVERRIDES]\n       bop-gen generate --config FILE --renderer BACKEND --output DATASET [--resume] [OVERRIDES]\n       bop-gen validate DATASET\n       bop-gen export bop DATASET --output DIRECTORY [--depth-scale MM]\n       bop-gen bench --config FILE --renderers BACKEND[,BACKEND] --output DIRECTORY [--repetitions N]\n       bop-gen optimize-asset SOURCE --output FILE.glb [--max-deviation-mm MM] [--crease-angle DEG] [--unit-scale M]\n\nOVERRIDES: --seed N --scenes N (written into the resolved config)\nGlobal: bop-gen [--log-format human|json] SUBCOMMAND; filter logs with BOP_LOG (default info)";

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

#[derive(Clone, Debug, PartialEq)]
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
        overrides: Overrides,
    },
    Render {
        config: PathBuf,
        snapshots: PathBuf,
        renderer: RendererSelection,
        output: PathBuf,
        overrides: Overrides,
    },
    Generate {
        config: PathBuf,
        renderer: RendererSelection,
        output: PathBuf,
        resume: bool,
        overrides: Overrides,
    },
    Validate {
        dataset: PathBuf,
    },
    ExportBop {
        dataset: PathBuf,
        output: PathBuf,
        depth_scale_mm: Option<f64>,
    },
    Bench {
        config: PathBuf,
        renderers: Vec<RendererSelection>,
        output: PathBuf,
        repetitions: u32,
    },
    OptimizeAsset {
        source: PathBuf,
        output: PathBuf,
        max_deviation_mm: f32,
        crease_angle_deg: f32,
        unit_scale_m: Option<f32>,
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
            let options =
                parse_options(&rest, &["--config", "--output", "--seed", "--scenes"], &[])?;
            ensure_no_flags(&options.flags)?;
            Ok(Command::Simulate {
                config: required_path(&options.values, "--config")?,
                output: required_path(&options.values, "--output")?,
                overrides: Overrides::parse(&options.values)?,
            })
        }
        "render" => {
            let options = parse_options(
                &rest,
                &[
                    "--config",
                    "--snapshots",
                    "--renderer",
                    "--output",
                    "--seed",
                    "--scenes",
                ],
                &[],
            )?;
            ensure_no_flags(&options.flags)?;
            Ok(Command::Render {
                config: required_path(&options.values, "--config")?,
                snapshots: required_path(&options.values, "--snapshots")?,
                renderer: one_renderer(required(&options.values, "--renderer")?)?,
                output: required_path(&options.values, "--output")?,
                overrides: Overrides::parse(&options.values)?,
            })
        }
        "generate" => {
            let options = parse_options(
                &rest,
                &["--config", "--renderer", "--output", "--seed", "--scenes"],
                &["--resume"],
            )?;
            Ok(Command::Generate {
                config: required_path(&options.values, "--config")?,
                renderer: one_renderer(required(&options.values, "--renderer")?)?,
                output: required_path(&options.values, "--output")?,
                resume: options.flags.contains("--resume"),
                overrides: Overrides::parse(&options.values)?,
            })
        }
        "validate" if rest.len() == 1 => Ok(Command::Validate {
            dataset: PathBuf::from(&rest[0]),
        }),
        "export" if rest.first().map(String::as_str) == Some("bop") => parse_export_bop(&rest),
        "bench" => parse_bench(&rest),
        "optimize-asset" => parse_optimize_asset(&rest),
        _ => Err(format!("unknown or malformed subcommand {command:?}")),
    }
}

fn parse_export_bop(rest: &[String]) -> Result<Command, String> {
    let dataset = rest
        .get(1)
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| "export bop requires DATASET".to_owned())?;
    let options = parse_options(&rest[2..], &["--output", "--depth-scale"], &[])?;
    ensure_no_flags(&options.flags)?;
    let depth_scale_mm = options
        .values
        .get("--depth-scale")
        .map(|value| {
            value
                .parse::<f64>()
                .ok()
                .filter(|scale| scale.is_finite() && *scale > 0.0)
                .ok_or_else(|| format!("--depth-scale must be a positive number, not {value:?}"))
        })
        .transpose()?;
    Ok(Command::ExportBop {
        dataset: PathBuf::from(dataset),
        output: required_path(&options.values, "--output")?,
        depth_scale_mm,
    })
}

fn parse_optimize_asset(rest: &[String]) -> Result<Command, String> {
    let source = rest
        .first()
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| "optimize-asset requires SOURCE".to_owned())?;
    let options = parse_options(
        &rest[1..],
        &[
            "--output",
            "--max-deviation-mm",
            "--crease-angle",
            "--unit-scale",
        ],
        &[],
    )?;
    ensure_no_flags(&options.flags)?;
    let number = |name: &str, default: Option<f32>| -> Result<Option<f32>, String> {
        options.values.get(name).map_or(Ok(default), |value| {
            value
                .parse::<f32>()
                .ok()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(Some)
                .ok_or_else(|| format!("{name} must be a nonnegative number, not {value:?}"))
        })
    };
    Ok(Command::OptimizeAsset {
        source: PathBuf::from(source),
        output: required_path(&options.values, "--output")?,
        max_deviation_mm: number("--max-deviation-mm", Some(0.1))?.unwrap_or_default(),
        crease_angle_deg: number("--crease-angle", Some(30.0))?.unwrap_or_default(),
        unit_scale_m: number("--unit-scale", None)?,
    })
}

fn parse_bench(rest: &[String]) -> Result<Command, String> {
    let options = parse_options(
        rest,
        &["--config", "--renderers", "--output", "--repetitions"],
        &[],
    )?;
    ensure_no_flags(&options.flags)?;
    let renderers = required(&options.values, "--renderers")?
        .split(',')
        .map(one_renderer)
        .collect::<Result<Vec<_>, _>>()?;
    if renderers.is_empty() {
        return Err("--renderers must not be empty".to_owned());
    }
    if (1..renderers.len()).any(|index| renderers[..index].contains(&renderers[index])) {
        return Err("--renderers must not repeat a backend".to_owned());
    }
    let repetitions = options.values.get("--repetitions").map_or(Ok(5), |value| {
        value
            .parse::<u32>()
            .ok()
            .filter(|count| *count > 0)
            .ok_or_else(|| format!("--repetitions must be a positive integer, not {value:?}"))
    })?;
    Ok(Command::Bench {
        config: required_path(&options.values, "--config")?,
        renderers,
        output: required_path(&options.values, "--output")?,
        repetitions,
    })
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
            println!("{}", bop_core::run_config_json_schema());
            ExitCode::SUCCESS
        }
        Command::PrepareAssets { config } => prepare_assets(&config),
        Command::Simulate {
            config,
            output,
            overrides,
        } => simulate(&config, &output, overrides),
        Command::Render {
            config,
            snapshots,
            renderer,
            output,
            overrides,
        } => render(&config, &snapshots, renderer, &output, overrides),
        Command::Generate {
            config,
            renderer,
            output,
            resume,
            overrides,
        } => generate(&config, renderer, &output, resume, overrides),
        Command::Validate { dataset } => validate(&dataset),
        Command::ExportBop {
            dataset,
            output,
            depth_scale_mm,
        } => export_bop(&dataset, &output, depth_scale_mm),
        Command::Bench {
            config,
            renderers,
            output,
            repetitions,
        } => bench::bench(&config, &renderers, &output, repetitions),
        Command::OptimizeAsset {
            source,
            output,
            max_deviation_mm,
            crease_angle_deg,
            unit_scale_m,
        } => optimize_asset(
            &source,
            &output,
            max_deviation_mm,
            crease_angle_deg,
            unit_scale_m,
        ),
    }
}

/// Welds and simplifies an STL or GLB within a deviation budget into a GLB.
fn optimize_asset(
    source: &std::path::Path,
    output: &std::path::Path,
    max_deviation_mm: f32,
    crease_angle_deg: f32,
    unit_scale_m: Option<f32>,
) -> ExitCode {
    let is_stl = source
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("stl"));
    let unit_scale_m = match (is_stl, unit_scale_m) {
        (true, None) => {
            return configuration_error(
                "STL has no units: pass --unit-scale (0.001 for millimetres)",
            );
        }
        (_, scale) => scale.unwrap_or(1.0),
    };
    if output.exists() {
        return configuration_error(&format!("{} already exists", output.display()));
    }
    let options = match bop_pipeline::MeshOptimizationOptions::try_new(
        max_deviation_mm / 1000.0,
        crease_angle_deg,
    ) {
        Ok(options) => options,
        Err(error) => return configuration_error(&error.to_string()),
    };
    let bytes = match std::fs::read(source) {
        Ok(bytes) => bytes,
        Err(error) => {
            return configuration_error(&format!("cannot read {}: {error}", source.display()));
        }
    };
    let optimized = match bop_pipeline::optimize_asset_source(source, &bytes, unit_scale_m, options)
    {
        Ok(optimized) => optimized,
        Err(error) => return configuration_error(&error.to_string()),
    };
    if let Err(error) = std::fs::write(output, &optimized.glb) {
        tracing::error!("cannot write {}: {error}", output.display());
        return ExitCode::from(5);
    }
    let report = optimized.report;
    println!(
        "{}",
        serde_json::json!({
            "output": output,
            "source_bytes": bytes.len(),
            "bytes": optimized.glb.len(),
            "source_triangles": report.source_triangles,
            "triangles": report.triangles,
            "source_vertices": report.source_vertices,
            "vertices": report.vertices,
            "max_deviation_mm": report.max_deviation_m * 1000.0,
            "dropped_tex_coord_sets": report.dropped_tex_coord_sets,
        })
    );
    ExitCode::SUCCESS
}

fn validate(dataset: &std::path::Path) -> ExitCode {
    match bop_dataset::validate_dataset(dataset) {
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
            tracing::error!("dataset validation failed: {error}");
            ExitCode::from(5)
        }
    }
}

fn export_bop(
    dataset: &std::path::Path,
    output: &std::path::Path,
    depth_scale_mm: Option<f64>,
) -> ExitCode {
    let options = match depth_scale_mm.map(bop_dataset::BopExportOptions::try_new) {
        None => bop_dataset::BopExportOptions::default(),
        Some(Ok(options)) => options,
        Some(Err(error)) => return configuration_error(&error.to_string()),
    };
    match bop_dataset::export_bop(dataset, output, options) {
        Ok(report) => {
            tracing::info!(
                output = %output.display(),
                scenes = report.scene_count,
                images = report.image_count,
                "exported BOP dataset"
            );
            println!(
                "{}",
                serde_json::json!({
                    "object_count": report.object_count,
                    "scene_count": report.scene_count,
                    "image_count": report.image_count,
                    "ground_truth_count": report.ground_truth_count,
                    "depth_scale": options.depth_scale_mm(),
                })
            );
            ExitCode::SUCCESS
        }
        Err(bop_dataset::BopExportError::OutputExists { path }) => {
            configuration_error(&format!("export output {} already exists", path.display()))
        }
        Err(error) => {
            tracing::error!("BOP export failed: {error}");
            ExitCode::from(5)
        }
    }
}

fn prepare_assets(config_path: &std::path::Path) -> ExitCode {
    let loaded = match load_run(config_path, Overrides::default()) {
        Ok(loaded) => loaded,
        Err(error) => return configuration_error(&error),
    };
    match bop_pipeline::prepare_asset_catalog(loaded.config().plan(), loaded.directory()) {
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
            tracing::error!("asset preparation failed: {error}");
            ExitCode::from(2)
        }
    }
}

fn load_run(config_path: &std::path::Path, overrides: Overrides) -> Result<LoadedRun, String> {
    let bytes = std::fs::read(config_path)
        .map_err(|error| format!("cannot read config {}: {error}", config_path.display()))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| format!("config {} is not UTF-8: {error}", config_path.display()))?;
    let resolved = overrides.apply(text).map_err(|error| {
        format!(
            "cannot apply overrides to {}: {error}",
            config_path.display()
        )
    })?;
    let bytes = if overrides.is_empty() {
        bytes
    } else {
        resolved.into_bytes()
    };
    let directory = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_owned();
    let loaded = LoadedRun::parse(bytes, directory)
        .map_err(|error| format!("invalid config {}: {error}", config_path.display()))?;
    let run = loaded.config().plan().run();
    tracing::info!(
        config = %config_path.display(),
        seed = run.master_seed(),
        scenes = run.valid_scene_count(),
        simulation_workers = run.simulation_workers(),
        renderer_workers = run.renderer_workers(),
        overridden = !overrides.is_empty(),
        "loaded run configuration"
    );
    Ok(loaded)
}

fn configuration_error(message: &str) -> ExitCode {
    tracing::error!("{message}");
    ExitCode::from(2)
}

fn simulate(
    config_path: &std::path::Path,
    output: &std::path::Path,
    overrides: Overrides,
) -> ExitCode {
    let loaded = match load_run(config_path, overrides) {
        Ok(loaded) => loaded,
        Err(error) => return configuration_error(&error),
    };
    let prepared =
        match bop_pipeline::prepare_asset_catalog(loaded.config().plan(), loaded.directory()) {
            Ok(prepared) => prepared,
            Err(error) => {
                return configuration_error(&format!("asset preparation failed: {error}"));
            }
        };
    let plan = loaded.config().plan();
    let fingerprint = prepared.visual_assets().fingerprint();
    let cancellation = interrupt::cancel_on_interrupt();
    let result = if plan.run().simulation_workers() > 1 {
        match bop_pipeline::ParallelSnapshotSimulator::try_new(
            plan,
            prepared.shared_physics_assets(),
        ) {
            Ok(simulator) => simulator.run(loaded.bytes(), fingerprint, output, &cancellation),
            Err(error) => {
                return configuration_error(&format!("simulation setup failed: {error}"));
            }
        }
    } else {
        match bop_pipeline::SerialSnapshotSimulator::try_new(plan, prepared.shared_physics_assets())
        {
            Ok(simulator) => simulator.run(loaded.bytes(), fingerprint, output, &cancellation),
            Err(error) => {
                return configuration_error(&format!("simulation setup failed: {error}"));
            }
        }
    };
    match result {
        Ok(report) => print_run_report(report, "rerun the same command"),
        Err(error) => {
            tracing::error!("simulation did not produce the requested corpus: {error}");
            ExitCode::from(4)
        }
    }
}

fn render(
    config_path: &std::path::Path,
    snapshots: &std::path::Path,
    renderer: RendererSelection,
    output: &std::path::Path,
    overrides: Overrides,
) -> ExitCode {
    let loaded = match load_run(config_path, overrides) {
        Ok(loaded) => loaded,
        Err(error) => return configuration_error(&error),
    };
    let prepared =
        match bop_pipeline::prepare_asset_catalog(loaded.config().plan(), loaded.directory()) {
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
    overrides: Overrides,
) -> ExitCode {
    let loaded = match load_run(config_path, overrides) {
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
        match bop_pipeline::prepare_asset_catalog(loaded.config().plan(), loaded.directory()) {
            Ok(prepared) => prepared,
            Err(error) => {
                return configuration_error(&format!("asset preparation failed: {error}"));
            }
        };
    let renderer = match renderer {
        RendererSelection::Wgpu => bop_run::RendererKind::Wgpu,
        RendererSelection::BlenderProc => bop_run::RendererKind::BlenderProc,
        RendererSelection::All => unreachable!("parser rejects all for generate"),
    };
    let options = bop_run::GenerateOptions {
        cancellation: interrupt::cancel_on_interrupt(),
        observer: None,
        blenderproc: blenderproc_runtime(),
    };
    match bop_run::generate(&loaded, &prepared, renderer, output, &options) {
        Ok(report) => print_run_report(report, "rerun the same command with --resume"),
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::from(error.exit_code())
        }
    }
}

#[cfg(feature = "wgpu")]
fn render_wgpu(
    loaded: &LoadedRun,
    prepared: &bop_pipeline::PreparedAssetCatalog,
    snapshots: &std::path::Path,
    output: &std::path::Path,
) -> ExitCode {
    match renderer_pool(loaded, |index, _| {
        bop_render_wgpu::WgpuRenderer::try_new_for_worker(
            loaded.config().plan().renderers().wgpu(),
            index,
        )
    }) {
        Ok(renderers) => run_render(loaded, prepared, snapshots, output, renderers),
        Err(error) => unavailable(format!("wgpu unavailable: {error}")),
    }
}

#[cfg(not(feature = "wgpu"))]
fn render_wgpu(
    _loaded: &LoadedRun,
    _prepared: &bop_pipeline::PreparedAssetCatalog,
    _snapshots: &std::path::Path,
    _output: &std::path::Path,
) -> ExitCode {
    unavailable("wgpu is unavailable in this build")
}

#[cfg(feature = "blenderproc")]
fn render_blenderproc(
    loaded: &LoadedRun,
    prepared: &bop_pipeline::PreparedAssetCatalog,
    snapshots: &std::path::Path,
    output: &std::path::Path,
) -> ExitCode {
    match renderer_pool(loaded, |index, count| {
        bop_render_blenderproc::BlenderProcRenderer::try_new_for_worker(
            resolved_blenderproc_config(loaded),
            index,
            count,
        )
    }) {
        Ok(renderers) => run_render(loaded, prepared, snapshots, output, renderers),
        Err(error) => unavailable(format!("BlenderProc unavailable: {error}")),
    }
}

#[cfg(not(feature = "blenderproc"))]
fn render_blenderproc(
    _loaded: &LoadedRun,
    _prepared: &bop_pipeline::PreparedAssetCatalog,
    _snapshots: &std::path::Path,
    _output: &std::path::Path,
) -> ExitCode {
    unavailable("BlenderProc is unavailable in this build")
}

#[cfg(any(feature = "wgpu", feature = "blenderproc"))]
fn run_render<R>(
    loaded: &LoadedRun,
    prepared: &bop_pipeline::PreparedAssetCatalog,
    snapshots: &std::path::Path,
    output: &std::path::Path,
    mut renderers: Vec<R>,
) -> ExitCode
where
    R: bop_render_api::Renderer,
{
    let plan = loaded.config().plan();
    let catalog = prepared.visual_assets();
    let cancellation = interrupt::cancel_on_interrupt();
    let result = if renderers.len() > 1 {
        bop_pipeline::ParallelSnapshotRenderer::try_new(plan, renderers, output).and_then(
            |mut pipeline| {
                pipeline.run_until_cancelled(loaded.bytes(), catalog, snapshots, &cancellation)
            },
        )
    } else {
        let renderer = renderers
            .pop()
            .expect("renderer_pool builds at least one renderer");
        bop_pipeline::SerialSnapshotRenderer::new(plan, renderer, output).run_until_cancelled(
            loaded.bytes(),
            catalog,
            snapshots,
            &cancellation,
        )
    };
    match result {
        Ok(report) => print_run_report(report, "rerun the same command"),
        Err(error) => {
            tracing::error!("render failed: {error}");
            match error {
                bop_pipeline::SnapshotRenderError::Render(_)
                | bop_pipeline::SnapshotRenderError::RendererLifecycle(_) => ExitCode::from(6),
                _ => ExitCode::from(5),
            }
        }
    }
}

/// Prints the run result; an interrupted run exits with status 130.
fn print_run_report(report: bop_pipeline::RunReport, resume_hint: &str) -> ExitCode {
    tracing::info!(
        preexisting_scenes = report.preexisting_scene_count(),
        generated_scenes = report.generated_scene_count(),
        total_scenes = report.total_scene_count(),
        cancelled = report.was_cancelled(),
        "run finished"
    );
    println!(
        "{}",
        serde_json::json!({
            "preexisting_scenes": report.preexisting_scene_count(),
            "generated_scenes": report.generated_scene_count(),
            "total_scenes": report.total_scene_count(),
            "cancelled": report.was_cancelled(),
        })
    );
    if report.was_cancelled() {
        tracing::warn!("run interrupted; committed scenes are kept, {resume_hint} to continue");
        ExitCode::from(interrupt::INTERRUPTED)
    } else {
        ExitCode::SUCCESS
    }
}

fn unavailable(message: impl std::fmt::Display) -> ExitCode {
    tracing::error!("{message}");
    ExitCode::from(3)
}

/// `BlenderProc` overrides from the environment: `BOP_BLENDERPROC_EXECUTABLE`
/// replaces the configured executable.
fn blenderproc_runtime() -> bop_run::BlenderProcRuntime {
    bop_run::BlenderProcRuntime {
        executable: std::env::var_os("BOP_BLENDERPROC_EXECUTABLE").map(PathBuf::from),
        worker: None,
    }
}

#[cfg(feature = "blenderproc")]
fn resolved_blenderproc_config(loaded: &LoadedRun) -> bop_core::BlenderProcRendererConfig {
    bop_run::resolved_blenderproc_config(loaded, &blenderproc_runtime())
}

fn doctor(renderer: &str) -> ExitCode {
    match renderer {
        "wgpu" => doctor_wgpu(),
        "blenderproc" => doctor_blenderproc(),
        _ => {
            tracing::error!("renderer {renderer:?} is unavailable in this build");
            ExitCode::from(3)
        }
    }
}

#[cfg(feature = "wgpu")]
fn doctor_wgpu() -> ExitCode {
    let config = bop_core::WgpuRendererConfig::try_new(
        bop_core::WgpuDevicePreference::Automatic,
        bop_core::WgpuAdapterPreference::HighPerformance,
        2048,
        false,
    )
    .expect("fixed doctor configuration");
    match bop_render_wgpu::WgpuRenderer::try_new(config) {
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
            tracing::error!("wgpu unavailable: {error}");
            ExitCode::from(3)
        }
    }
}

#[cfg(not(feature = "wgpu"))]
fn doctor_wgpu() -> ExitCode {
    tracing::error!("renderer \"wgpu\" is unavailable in this build");
    ExitCode::from(3)
}

/// Finds the `BlenderProc` worker script for `doctor`, which has no run config.
///
/// Order: `BOP_BLENDERPROC_WORKER`, then `blenderproc-worker/` beside the
/// executable (the release archive layout), then this source checkout.
#[cfg(feature = "blenderproc")]
fn default_worker_path() -> PathBuf {
    if let Some(path) = std::env::var_os("BOP_BLENDERPROC_WORKER") {
        return PathBuf::from(path);
    }
    let bundled = std::env::current_exe().ok().and_then(|executable| {
        executable
            .parent()
            .map(|directory| directory.join("blenderproc-worker/blenderproc_worker.py"))
    });
    match bundled {
        Some(path) if path.is_file() => path,
        _ => std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/blenderproc-worker/blenderproc_worker.py"),
    }
}

#[cfg(feature = "blenderproc")]
fn doctor_blenderproc() -> ExitCode {
    let executable =
        std::env::var_os("BOP_BLENDERPROC_EXECUTABLE").unwrap_or_else(|| "blenderproc".into());
    let worker = default_worker_path();
    let config = bop_core::BlenderProcRendererConfig::try_new(
        executable, worker, "AUTO", 30.0, 300.0, 65_536,
    )
    .expect("fixed doctor configuration");
    match bop_render_blenderproc::BlenderProcRenderer::try_new(config) {
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
            tracing::error!("BlenderProc unavailable: {error}");
            ExitCode::from(3)
        }
    }
}

#[cfg(not(feature = "blenderproc"))]
fn doctor_blenderproc() -> ExitCode {
    tracing::error!("renderer \"blenderproc\" is unavailable in this build");
    ExitCode::from(3)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "wgpu")]
    use std::fs;
    use std::path::PathBuf;

    use super::{Command, LogFormat, Overrides, RendererSelection, parse, run, take_log_format};

    #[test]
    fn run_identity_overrides_parse_and_rewrite_the_run_table() {
        let Ok(Command::Simulate { overrides, .. }) = parse([
            "simulate", "--config", "run.toml", "--output", "corpus", "--seed", "42", "--scenes",
            "7",
        ]) else {
            panic!("simulate with overrides parses");
        };
        assert_eq!(
            overrides,
            Overrides {
                seed: Some(42),
                scenes: Some(7)
            }
        );
        let rewritten = overrides
            .apply("[run]\nmaster_seed = 1\nvalid_scene_count = 2\n")
            .unwrap();
        assert!(rewritten.contains("master_seed = 42"));
        assert!(rewritten.contains("valid_scene_count = 7"));
        assert_eq!(
            Overrides::default()
                .apply("[run] # kept verbatim\n")
                .unwrap(),
            "[run] # kept verbatim\n"
        );
        assert!(
            Overrides {
                seed: Some(u64::MAX),
                scenes: None
            }
            .apply("[run]\n")
            .is_err()
        );
        assert!(parse(["simulate", "--config", "c", "--output", "o", "--seed", "-1"]).is_err());
    }

    #[test]
    fn a_leading_log_format_option_is_consumed() {
        let mut arguments = vec![
            "--log-format".to_owned(),
            "json".to_owned(),
            "schema".to_owned(),
        ];
        assert_eq!(take_log_format(&mut arguments), Ok(LogFormat::Json));
        assert_eq!(arguments, vec!["schema".to_owned()]);
        let mut arguments = vec!["schema".to_owned()];
        assert_eq!(take_log_format(&mut arguments), Ok(LogFormat::Human));
        let mut arguments = vec!["--log-format".to_owned(), "xml".to_owned()];
        assert!(take_log_format(&mut arguments).is_err());
    }

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
                output: PathBuf::from("corpus"),
                overrides: Overrides::default()
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
                output: PathBuf::from("dataset"),
                overrides: Overrides::default()
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
                resume: true,
                overrides: Overrides::default()
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
                output: PathBuf::from("bop-out"),
                depth_scale_mm: None,
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
                output: PathBuf::from("reports"),
                repetitions: 5,
            })
        );
    }

    #[test]
    fn export_bop_parses_an_optional_positive_depth_scale() {
        assert_eq!(
            parse([
                "export",
                "bop",
                "dataset",
                "--output",
                "bop-out",
                "--depth-scale",
                "1"
            ]),
            Ok(Command::ExportBop {
                dataset: PathBuf::from("dataset"),
                output: PathBuf::from("bop-out"),
                depth_scale_mm: Some(1.0),
            })
        );
        assert!(
            parse([
                "export",
                "bop",
                "dataset",
                "--output",
                "bop-out",
                "--depth-scale",
                "0"
            ])
            .is_err()
        );
    }

    #[test]
    fn optimize_asset_parses_defaults_and_rejects_negative_numbers() {
        assert_eq!(
            parse(["optimize-asset", "cup.stl", "--output", "cup.glb"]),
            Ok(Command::OptimizeAsset {
                source: PathBuf::from("cup.stl"),
                output: PathBuf::from("cup.glb"),
                max_deviation_mm: 0.1,
                crease_angle_deg: 30.0,
                unit_scale_m: None,
            })
        );
        assert_eq!(
            parse([
                "optimize-asset",
                "cup.stl",
                "--output",
                "cup.glb",
                "--max-deviation-mm",
                "0",
                "--unit-scale",
                "0.001"
            ]),
            Ok(Command::OptimizeAsset {
                source: PathBuf::from("cup.stl"),
                output: PathBuf::from("cup.glb"),
                max_deviation_mm: 0.0,
                crease_angle_deg: 30.0,
                unit_scale_m: Some(0.001),
            })
        );
        assert!(parse(["optimize-asset", "--output", "cup.glb"]).is_err());
        assert!(
            parse([
                "optimize-asset",
                "cup.stl",
                "--output",
                "cup.glb",
                "--max-deviation-mm",
                "-1"
            ])
            .is_err()
        );
    }

    #[test]
    fn parser_rejects_missing_duplicate_unknown_and_ambiguous_options() {
        for arguments in [
            ["--renderers", "wgpu,wgpu", "--repetitions", "5"],
            ["--renderers", "wgpu", "--repetitions", "0"],
        ] {
            let mut full = vec!["bench", "--config", "run.toml", "--output", "reports"];
            full.extend(arguments);
            assert!(parse(full).is_err());
        }
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
            "bop-missing-config-{}-{}.toml",
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
            "bop-missing-simulation-config-{}.toml",
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
            "bop-missing-render-config-{}.toml",
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
        let config = include_str!("../../bop-pipeline/tests/fixtures/serial_run.toml")
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

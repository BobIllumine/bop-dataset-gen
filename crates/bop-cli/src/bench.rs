//! `bop-gen bench`: measures one preset across renderer backends.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

use bop_assets::ResolvedAssetCatalog;
use bop_core::{BackendKind, SceneSnapshot};
use bop_pipeline::{
    BackendBenchmark, BenchmarkBackend, BenchmarkHost, BenchmarkReport, PreparationBenchmark,
    RepetitionSample, repetition_order,
};

use crate::{LoadedRun, Overrides, RendererSelection, configuration_error, load_run};

/// Scenes rendered by each backend before measuring; they are not counted.
const WARMUP_SCENES: usize = 2;

pub(crate) fn bench(
    config_path: &Path,
    renderers: &[RendererSelection],
    output: &Path,
    repetitions: u32,
) -> ExitCode {
    let loaded = match load_run(config_path, Overrides::default()) {
        Ok(loaded) => loaded,
        Err(error) => return configuration_error(&error),
    };
    let plan = loaded.config().plan();
    let cache = plan.run().cache_path();
    let cache_preexisting = if cache.is_absolute() {
        cache.exists()
    } else {
        loaded.directory().join(cache).exists()
    };
    let (prepared, preparation_seconds) = match prepare_twice(&loaded) {
        Ok(prepared) => prepared,
        Err(error) => return configuration_error(&format!("asset preparation failed: {error}")),
    };
    if let Err(error) = std::fs::create_dir_all(output) {
        return configuration_error(&format!("cannot create {}: {error}", output.display()));
    }

    let (scenes, physics) = match bop_pipeline::benchmark_corpus(
        plan,
        loaded.bytes(),
        &prepared,
        &output.join("corpus"),
    ) {
        Ok(corpus) => corpus,
        Err(error) => {
            tracing::error!("benchmark corpus failed: {error}");
            return ExitCode::from(4);
        }
    };
    tracing::info!(
        scenes = physics.scene_count,
        reused = physics.reused_corpus,
        "benchmark corpus ready"
    );

    let catalog = prepared.visual_assets();
    let mut backends = Vec::with_capacity(renderers.len());
    for selection in renderers {
        match backend(*selection, &loaded, catalog, &scenes, output) {
            Ok(backend) => backends.push(backend),
            Err(code) => return code,
        }
    }
    let (samples, order) = match measure_all(&mut backends, plan.run().master_seed(), repetitions) {
        Ok(measured) => measured,
        Err(code) => return code,
    };
    let summaries = match backends
        .iter()
        .zip(samples)
        .map(|(backend, samples)| BackendBenchmark::try_new(backend.as_ref(), samples))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(summaries) => summaries,
        Err(error) => return configuration_error(&error.to_string()),
    };
    // Dropping the renderers shuts down worker processes, so their peak RSS is
    // known. Read it before `host` spawns git and sysctl, which are children too.
    drop(backends);
    let rss = peak_rss();

    let report = BenchmarkReport::new(
        config_path.file_stem().map_or_else(
            || "benchmark".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        ),
        bop_dataset::fingerprint_config(loaded.bytes()),
        plan.cameras().len(),
        order,
        host(rss),
        PreparationBenchmark {
            first_seconds: preparation_seconds[0],
            repeat_seconds: preparation_seconds[1],
            cache_preexisting,
        },
        physics,
        summaries,
    );
    write_report(&report, output)
}

/// Prepares the assets twice: the first call may be cold, the second reuses the cache.
fn prepare_twice(
    loaded: &LoadedRun,
) -> Result<(bop_pipeline::PreparedAssetCatalog, [f64; 2]), bop_pipeline::AssetPreparationError> {
    let plan = loaded.config().plan();
    let started = Instant::now();
    bop_pipeline::prepare_asset_catalog(plan, loaded.directory())?;
    let first = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let prepared = bop_pipeline::prepare_asset_catalog(plan, loaded.directory())?;
    Ok((prepared, [first, started.elapsed().as_secs_f64()]))
}

type Measured = (Vec<Vec<RepetitionSample>>, Vec<Vec<BackendKind>>);

/// Runs every repetition, each in its seeded backend order.
fn measure_all(
    backends: &mut [Backend<'_>],
    seed: u64,
    repetitions: u32,
) -> Result<Measured, ExitCode> {
    let mut samples = vec![Vec::new(); backends.len()];
    let mut order = Vec::new();
    for repetition in 0..repetitions {
        let sequence = repetition_order(seed, repetition, backends.len());
        order.push(
            sequence
                .iter()
                .map(|index| backends[*index].info().kind())
                .collect(),
        );
        for (position, index) in (0_u32..).zip(&sequence) {
            let sample = backends[*index]
                .measure(repetition, position)
                .map_err(|error| {
                    tracing::error!("benchmark repetition failed: {error}");
                    ExitCode::from(6)
                })?;
            tracing::info!(
                backend = ?backends[*index].info().kind(),
                repetition,
                scenes_per_second = sample.scenes_per_second,
                "measured repetition"
            );
            samples[*index].push(sample);
        }
    }
    Ok((samples, order))
}

fn write_report(report: &BenchmarkReport, output: &Path) -> ExitCode {
    let json_path = output.join("report.json");
    let markdown_path = output.join("report.md");
    let mut json = serde_json::to_vec_pretty(report).expect("benchmark reports serialize");
    json.push(b'\n');
    for (path, bytes) in [
        (&json_path, json),
        (&markdown_path, report.to_markdown().into_bytes()),
    ] {
        if let Err(error) = std::fs::write(path, bytes) {
            tracing::error!("cannot write {}: {error}", path.display());
            return ExitCode::from(5);
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "report": json_path,
            "markdown": markdown_path,
            "backends": report.backends.iter().map(|backend| serde_json::json!({
                "kind": backend.backend.kind(),
                "scenes_per_second": backend.scenes_per_second_median,
                "frames_per_second": backend.frames_per_second_median,
            })).collect::<Vec<_>>(),
        })
    );
    ExitCode::SUCCESS
}

type Backend<'a> = Box<dyn BenchmarkBackend + 'a>;

fn backend<'a>(
    selection: RendererSelection,
    loaded: &'a LoadedRun,
    catalog: &'a ResolvedAssetCatalog,
    scenes: &[SceneSnapshot],
    output: &Path,
) -> Result<Backend<'a>, ExitCode> {
    let root = |name: &str| output.join("runs").join(name);
    match selection {
        RendererSelection::Wgpu => wgpu_backend(loaded, catalog, scenes, root("wgpu")),
        RendererSelection::BlenderProc => {
            blenderproc_backend(loaded, catalog, scenes, root("blenderproc"))
        }
        RendererSelection::All => unreachable!("parser rejects all for bench"),
    }
}

#[cfg(any(feature = "wgpu", feature = "blenderproc"))]
fn warmed<'a, R: bop_render_api::Renderer + 'a>(
    renderer: R,
    loaded: &'a LoadedRun,
    catalog: &'a ResolvedAssetCatalog,
    scenes: &[SceneSnapshot],
    root: PathBuf,
) -> Result<Backend<'a>, ExitCode> {
    bop_pipeline::RendererBenchmark::try_new(
        renderer,
        loaded.config().plan(),
        loaded.bytes(),
        catalog,
        scenes,
        root,
        WARMUP_SCENES,
    )
    .map(|backend| Box::new(backend) as Backend<'a>)
    .map_err(|error| {
        tracing::error!("benchmark warm-up failed: {error}");
        ExitCode::from(6)
    })
}

#[cfg(feature = "wgpu")]
fn wgpu_backend<'a>(
    loaded: &'a LoadedRun,
    catalog: &'a ResolvedAssetCatalog,
    scenes: &[SceneSnapshot],
    root: PathBuf,
) -> Result<Backend<'a>, ExitCode> {
    let renderer =
        bop_render_wgpu::WgpuRenderer::try_new(loaded.config().plan().renderers().wgpu())
            .map_err(|error| crate::unavailable(format!("wgpu unavailable: {error}")))?;
    warmed(renderer, loaded, catalog, scenes, root)
}

#[cfg(not(feature = "wgpu"))]
fn wgpu_backend<'a>(
    _loaded: &'a LoadedRun,
    _catalog: &'a ResolvedAssetCatalog,
    _scenes: &[SceneSnapshot],
    _root: PathBuf,
) -> Result<Backend<'a>, ExitCode> {
    Err(crate::unavailable("wgpu is unavailable in this build"))
}

#[cfg(feature = "blenderproc")]
fn blenderproc_backend<'a>(
    loaded: &'a LoadedRun,
    catalog: &'a ResolvedAssetCatalog,
    scenes: &[SceneSnapshot],
    root: PathBuf,
) -> Result<Backend<'a>, ExitCode> {
    let renderer = bop_render_blenderproc::BlenderProcRenderer::try_new(
        crate::resolved_blenderproc_config(loaded),
    )
    .map_err(|error| crate::unavailable(format!("BlenderProc unavailable: {error}")))?;
    warmed(renderer, loaded, catalog, scenes, root)
}

#[cfg(not(feature = "blenderproc"))]
fn blenderproc_backend<'a>(
    _loaded: &'a LoadedRun,
    _catalog: &'a ResolvedAssetCatalog,
    _scenes: &[SceneSnapshot],
    _root: PathBuf,
) -> Result<Backend<'a>, ExitCode> {
    Err(crate::unavailable(
        "BlenderProc is unavailable in this build",
    ))
}

/// Collects source, CPU, and memory provenance; unknown values stay `None`.
fn host((peak_rss_bytes, peak_child_rss_bytes): (Option<u64>, Option<u64>)) -> BenchmarkHost {
    let command = |program: &str, arguments: &[&str]| {
        std::process::Command::new(program)
            .args(arguments)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let git_revision = command("git", &["rev-parse", "HEAD"]);
    let git_dirty = git_revision
        .as_ref()
        .and_then(|_| command("git", &["status", "--porcelain"]))
        .map(|status| !status.is_empty());
    let cpu = if cfg!(target_os = "macos") {
        command("sysctl", &["-n", "machdep.cpu.brand_string"])
    } else {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|info| {
                info.lines()
                    .find_map(|line| line.strip_prefix("model name"))
                    .and_then(|rest| rest.split_once(':'))
                    .map(|(_, name)| name.trim().to_owned())
            })
    };
    BenchmarkHost {
        git_revision,
        git_dirty,
        cpu,
        logical_cpus: std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
        peak_rss_bytes,
        peak_child_rss_bytes,
    }
}

/// Peak RSS of this process and of its largest finished child, in bytes.
#[cfg(unix)]
fn peak_rss() -> (Option<u64>, Option<u64>) {
    use nix::sys::resource::{UsageWho, getrusage};
    // macOS reports bytes; other Unix systems report kilobytes.
    let unit = if cfg!(target_vendor = "apple") {
        1
    } else {
        1024
    };
    let peak = |who| {
        getrusage(who)
            .ok()
            .and_then(|usage| u64::try_from(usage.max_rss()).ok())
            .filter(|peak| *peak > 0)
            .map(|peak| peak * unit)
    };
    (peak(UsageWho::RUSAGE_SELF), peak(UsageWho::RUSAGE_CHILDREN))
}

#[cfg(not(unix))]
fn peak_rss() -> (Option<u64>, Option<u64>) {
    (None, None)
}

//! Benchmark driver: one fingerprinted snapshot corpus, then warmed renderer
//! backends measured over repeated passes in a seeded, alternating order.
//!
//! Physics is measured once, when the corpus is generated. Each renderer is
//! prepared once and warmed up on a few scenes that are not counted. Every
//! measured repetition renders the whole corpus into a fresh dataset with one
//! renderer instance, so backends are compared on equal work.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use bop_assets::ResolvedAssetCatalog;
use bop_core::{BackendInfo, RenderSceneRequest, RunPlan, SceneSnapshot};
use bop_dataset::{read_snapshot_corpus, sync_scene_index};
use bop_render_api::Renderer;
use serde::Serialize;
use thiserror::Error;

use crate::{
    CancellationToken, GenerationPipelineError, ParallelSnapshotSimulator, PipelineRenderError,
    PreparedAssetCatalog, RenderRequestPlanner, RenderRequestPlanningError, SerialDatasetRenderer,
    SerialSnapshotSimulator, SnapshotSimulationError, serial_generation::open_dataset_run,
};

/// Version of the JSON report layout.
pub const BENCHMARK_REPORT_SCHEMA_VERSION: u16 = 1;

/// Physics-stage figures for the benchmark corpus.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PhysicsBenchmark {
    /// Scenes in the corpus.
    pub scene_count: u64,
    /// Configured simulation workers.
    pub workers: u32,
    /// Whether an existing complete corpus was reused, so nothing was timed.
    pub reused_corpus: bool,
    /// Wall-clock seconds to simulate the whole corpus, when it was generated now.
    pub wall_seconds: Option<f64>,
    /// Scenes per second, when the corpus was generated now.
    pub scenes_per_second: Option<f64>,
    /// Mean fixed physics steps per accepted scene.
    pub steps_per_scene: f64,
    /// Rejected attempts divided by all attempts.
    pub rejection_rate: f64,
}

/// Simulates (or reuses) the benchmark corpus and summarizes its physics.
///
/// A complete compatible corpus already at `corpus_root` is reused, so
/// repeated benchmark runs render identical scenes; its physics is then
/// reported without timings.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when simulation fails or the corpus cannot be read.
pub fn benchmark_corpus(
    plan: &RunPlan,
    resolved_config_bytes: &[u8],
    prepared: &PreparedAssetCatalog,
    corpus_root: &Path,
) -> Result<(Vec<SceneSnapshot>, PhysicsBenchmark), BenchmarkError> {
    let fingerprint = prepared.visual_assets().fingerprint();
    let cancellation = CancellationToken::new();
    let workers = plan.run().simulation_workers();
    let started = Instant::now();
    let report = if workers > 1 {
        ParallelSnapshotSimulator::try_new(plan, prepared.shared_physics_assets())?.run(
            resolved_config_bytes,
            fingerprint,
            corpus_root,
            &cancellation,
        )?
    } else {
        SerialSnapshotSimulator::try_new(plan, prepared.shared_physics_assets())
            .map_err(SnapshotSimulationError::from)?
            .run(
                resolved_config_bytes,
                fingerprint,
                corpus_root,
                &cancellation,
            )?
    };
    let wall = started.elapsed().as_secs_f64();
    let scenes = read_snapshot_corpus(corpus_root).map_err(SnapshotSimulationError::from)?;
    let total = report.total_scene_count();
    let fresh = report.preexisting_scene_count() == 0;
    let attempts = scenes
        .iter()
        .map(|scene| u64::from(scene.physics().attempts()))
        .sum::<u64>();
    let steps = scenes
        .iter()
        .map(|scene| scene.physics().steps())
        .sum::<u64>();
    #[allow(clippy::cast_precision_loss)]
    let physics = PhysicsBenchmark {
        scene_count: total,
        workers,
        reused_corpus: report.generated_scene_count() == 0,
        wall_seconds: fresh.then_some(wall),
        scenes_per_second: fresh.then(|| total as f64 / wall),
        steps_per_scene: steps as f64 / total.max(1) as f64,
        rejection_rate: attempts.saturating_sub(total) as f64 / attempts.max(1) as f64,
    };
    Ok((scenes, physics))
}

/// One measured pass of a backend over the whole corpus.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RepetitionSample {
    /// Zero-based repetition index.
    pub repetition: u32,
    /// Position of this backend within the repetition's order.
    pub order_position: u32,
    /// Wall-clock seconds for rendering, encoding, and committing every scene.
    pub wall_seconds: f64,
    /// Scenes per second over the pass.
    pub scenes_per_second: f64,
    /// Frames (scene × camera) per second over the pass.
    pub frames_per_second: f64,
    /// Per-scene renderer-reported render seconds, summed.
    pub render_seconds: f64,
    /// Per-scene renderer-reported readback seconds, summed.
    pub readback_seconds: f64,
    /// Per-scene renderer-reported encode seconds, summed.
    pub encode_seconds: f64,
    /// Per-scene GPU timestamp seconds, summed, when the backend reports them.
    pub gpu_seconds: Option<f64>,
    /// Bytes written for the committed dataset.
    pub output_bytes: u64,
    /// Wall-clock seconds of each scene, in scene order.
    #[serde(skip)]
    pub scene_seconds: Vec<f64>,
}

/// A warmed renderer backend that can render the corpus repeatedly.
pub trait BenchmarkBackend {
    /// Backend provenance.
    fn info(&self) -> BackendInfo;
    /// Seconds the one-time `prepare` call took.
    fn prepare_seconds(&self) -> f64;
    /// Scenes rendered during warm-up and not counted.
    fn warmup_scenes(&self) -> usize;
    /// Renders every scene into a fresh dataset and returns the timings.
    ///
    /// # Errors
    ///
    /// Returns [`BenchmarkError`] when rendering or committing fails.
    fn measure(
        &mut self,
        repetition: u32,
        order_position: u32,
    ) -> Result<RepetitionSample, BenchmarkError>;
}

/// [`BenchmarkBackend`] over any [`Renderer`].
pub struct RendererBenchmark<'a, R> {
    dataset: SerialDatasetRenderer<R>,
    requests: Vec<RenderSceneRequest>,
    resolved_config_bytes: &'a [u8],
    catalog: &'a ResolvedAssetCatalog,
    work_root: PathBuf,
    prepare_seconds: f64,
    warmup_scenes: usize,
}

impl<'a, R: Renderer> RendererBenchmark<'a, R> {
    /// Prepares `renderer` and warms it up on the first `warmup_scenes` scenes.
    ///
    /// Datasets are written under `work_root`; only the latest measured
    /// repetition is kept.
    ///
    /// # Errors
    ///
    /// Returns [`BenchmarkError`] when request planning, preparation, or warm-up fails.
    pub fn try_new(
        renderer: R,
        plan: &RunPlan,
        resolved_config_bytes: &'a [u8],
        catalog: &'a ResolvedAssetCatalog,
        scenes: &[SceneSnapshot],
        work_root: PathBuf,
        warmup_scenes: usize,
    ) -> Result<Self, BenchmarkError> {
        let planner = RenderRequestPlanner::new(plan);
        let requests = scenes
            .iter()
            .map(|scene| planner.plan(scene.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        remove_tree(&work_root)?;
        let mut dataset = SerialDatasetRenderer::new(renderer, work_root.join("warmup"));
        let started = Instant::now();
        dataset
            .prepare(catalog)
            .map_err(PipelineRenderError::from)?;
        let prepare_seconds = started.elapsed().as_secs_f64();
        let mut benchmark = Self {
            dataset,
            requests,
            resolved_config_bytes,
            catalog,
            work_root,
            prepare_seconds,
            warmup_scenes: warmup_scenes.min(scenes.len()),
        };
        benchmark.render_into("warmup", benchmark.warmup_scenes)?;
        Ok(benchmark)
    }

    /// Renders the first `count` requests into `work_root/<name>`, replacing it.
    fn render_into(
        &mut self,
        name: &str,
        count: usize,
    ) -> Result<Vec<CompletedTiming>, BenchmarkError> {
        let root = self.work_root.join(name);
        remove_tree(&root)?;
        let total =
            u64::try_from(self.requests.len()).map_err(|_| BenchmarkError::TooManyScenes)?;
        open_dataset_run(
            &root,
            self.resolved_config_bytes,
            self.catalog,
            self.dataset.renderer().info(),
            total,
        )?;
        self.dataset.retarget(root.clone());
        let mut timings = Vec::with_capacity(count);
        for request in &self.requests[..count] {
            let started = Instant::now();
            let completed = self.dataset.render_and_commit(request)?;
            let stages = completed.report().timings();
            timings.push(CompletedTiming {
                wall_seconds: started.elapsed().as_secs_f64(),
                render_seconds: f64::from(stages.render_seconds().get()),
                readback_seconds: f64::from(stages.readback_seconds().get()),
                encode_seconds: f64::from(stages.encode_seconds().get()),
                gpu_seconds: stages.gpu_seconds().map(|value| f64::from(value.get())),
                frames: completed.report().camera_count().get(),
            });
        }
        self.dataset.flush().map_err(PipelineRenderError::from)?;
        sync_scene_index(&root).map_err(PipelineRenderError::from)?;
        Ok(timings)
    }
}

struct CompletedTiming {
    wall_seconds: f64,
    render_seconds: f64,
    readback_seconds: f64,
    encode_seconds: f64,
    gpu_seconds: Option<f64>,
    frames: u32,
}

impl<R: Renderer> BenchmarkBackend for RendererBenchmark<'_, R> {
    fn info(&self) -> BackendInfo {
        self.dataset.renderer().info()
    }

    fn prepare_seconds(&self) -> f64 {
        self.prepare_seconds
    }

    fn warmup_scenes(&self) -> usize {
        self.warmup_scenes
    }

    fn measure(
        &mut self,
        repetition: u32,
        order_position: u32,
    ) -> Result<RepetitionSample, BenchmarkError> {
        remove_tree(&self.work_root.join("warmup"))?;
        let name = format!("repetition-{repetition}");
        let started = Instant::now();
        let timings = self.render_into(&name, self.requests.len())?;
        let wall_seconds = started.elapsed().as_secs_f64();
        if repetition > 0 {
            remove_tree(
                &self
                    .work_root
                    .join(format!("repetition-{}", repetition - 1)),
            )?;
        }
        let frames = timings
            .iter()
            .map(|timing| u64::from(timing.frames))
            .sum::<u64>();
        let gpu = timings
            .iter()
            .map(|timing| timing.gpu_seconds)
            .sum::<Option<f64>>();
        #[allow(clippy::cast_precision_loss)]
        Ok(RepetitionSample {
            repetition,
            order_position,
            wall_seconds,
            scenes_per_second: timings.len() as f64 / wall_seconds,
            frames_per_second: frames as f64 / wall_seconds,
            render_seconds: timings.iter().map(|timing| timing.render_seconds).sum(),
            readback_seconds: timings.iter().map(|timing| timing.readback_seconds).sum(),
            encode_seconds: timings.iter().map(|timing| timing.encode_seconds).sum(),
            gpu_seconds: gpu,
            output_bytes: tree_size(&self.work_root.join(&name))?,
            scene_seconds: timings.iter().map(|timing| timing.wall_seconds).collect(),
        })
    }
}

/// Backend order for each repetition: a seeded rotation that advances by one
/// each repetition, so every backend leads equally often and two backends
/// alternate.
#[must_use]
pub fn repetition_order(seed: u64, repetition: u32, backends: usize) -> Vec<usize> {
    if backends == 0 {
        return Vec::new();
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bpp.bench.order.v1");
    hasher.update(&seed.to_le_bytes());
    let hash = hasher.finalize();
    let offset = hash.as_bytes().iter().take(8).fold(0_usize, |value, byte| {
        value.wrapping_shl(8) | usize::from(*byte)
    });
    let repetition = usize::try_from(repetition).unwrap_or(usize::MAX);
    let start = (offset % backends + repetition % backends) % backends;
    (0..backends)
        .map(|index| (start + index) % backends)
        .collect()
}

/// Summary of one backend's measured repetitions.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BackendBenchmark {
    /// Backend provenance.
    pub backend: BackendInfo,
    /// Seconds for the one-time renderer `prepare`.
    pub prepare_seconds: f64,
    /// Scenes rendered during warm-up and not counted.
    pub warmup_scenes: usize,
    /// Median scenes per second across repetitions.
    pub scenes_per_second_median: f64,
    /// Median frames per second across repetitions.
    pub frames_per_second_median: f64,
    /// Nearest-rank median scene latency over all measured scenes.
    pub scene_latency_p50_seconds: f64,
    /// Nearest-rank p95 scene latency over all measured scenes.
    pub scene_latency_p95_seconds: f64,
    /// Median per-scene renderer-reported render seconds.
    pub render_seconds_per_scene: f64,
    /// Median per-scene renderer-reported readback seconds.
    pub readback_seconds_per_scene: f64,
    /// Median per-scene renderer-reported encode seconds.
    pub encode_seconds_per_scene: f64,
    /// Median per-scene GPU seconds, when reported.
    pub gpu_seconds_per_scene: Option<f64>,
    /// Bytes of one committed dataset.
    pub output_bytes: u64,
    /// Every measured repetition.
    pub repetitions: Vec<RepetitionSample>,
}

impl BackendBenchmark {
    /// Summarizes measured repetitions.
    ///
    /// # Errors
    ///
    /// Returns [`BenchmarkError::NoSamples`] when `repetitions` is empty.
    pub fn try_new(
        backend: &dyn BenchmarkBackend,
        repetitions: Vec<RepetitionSample>,
    ) -> Result<Self, BenchmarkError> {
        let last = repetitions.last().ok_or(BenchmarkError::NoSamples)?;
        let output_bytes = last.output_bytes;
        #[allow(clippy::cast_precision_loss)]
        let scenes = last.scene_seconds.len().max(1) as f64;
        let per_scene = |value: fn(&RepetitionSample) -> f64| {
            median(repetitions.iter().map(value).collect()) / scenes
        };
        let latencies = repetitions
            .iter()
            .flat_map(|sample| sample.scene_seconds.iter().copied())
            .collect::<Vec<_>>();
        let gpu = repetitions
            .iter()
            .map(|sample| sample.gpu_seconds)
            .collect::<Option<Vec<_>>>()
            .map(|values| median(values) / scenes);
        Ok(Self {
            backend: backend.info(),
            prepare_seconds: backend.prepare_seconds(),
            warmup_scenes: backend.warmup_scenes(),
            scenes_per_second_median: median(
                repetitions
                    .iter()
                    .map(|sample| sample.scenes_per_second)
                    .collect(),
            ),
            frames_per_second_median: median(
                repetitions
                    .iter()
                    .map(|sample| sample.frames_per_second)
                    .collect(),
            ),
            scene_latency_p50_seconds: percentile(latencies.clone(), 50),
            scene_latency_p95_seconds: percentile(latencies, 95),
            render_seconds_per_scene: per_scene(|sample| sample.render_seconds),
            readback_seconds_per_scene: per_scene(|sample| sample.readback_seconds),
            encode_seconds_per_scene: per_scene(|sample| sample.encode_seconds),
            gpu_seconds_per_scene: gpu,
            output_bytes,
            repetitions,
        })
    }
}

/// Upper median.
fn median(values: Vec<f64>) -> f64 {
    percentile(values, 50)
}

/// Nearest-rank percentile; zero for no values.
fn percentile(mut values: Vec<f64>, percent: usize) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    if percent == 50 {
        return values[values.len() / 2];
    }
    let rank = (values.len() * percent).div_ceil(100).saturating_sub(1);
    values[rank]
}

fn remove_tree(path: &Path) -> Result<(), BenchmarkError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BenchmarkError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn tree_size(path: &Path) -> Result<u64, BenchmarkError> {
    let io = |source| BenchmarkError::Io {
        path: path.to_owned(),
        source,
    };
    let mut total = 0;
    for entry in fs::read_dir(path).map_err(io)? {
        let entry = entry.map_err(io)?;
        let metadata = entry.metadata().map_err(io)?;
        total += if metadata.is_dir() {
            tree_size(&entry.path())?
        } else {
            metadata.len()
        };
    }
    Ok(total)
}

/// A benchmark could not complete.
#[derive(Debug, Error)]
pub enum BenchmarkError {
    /// Corpus simulation or reading failed.
    #[error(transparent)]
    Simulation(#[from] SnapshotSimulationError),
    /// Render request planning failed.
    #[error(transparent)]
    Request(#[from] RenderRequestPlanningError),
    /// Rendering or committing failed.
    #[error(transparent)]
    Render(#[from] PipelineRenderError),
    /// A measurement dataset could not be opened.
    #[error(transparent)]
    Dataset(#[from] GenerationPipelineError),
    /// Filesystem access failed.
    #[error("benchmark I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// No repetitions were measured.
    #[error("a benchmark needs at least one measured repetition")]
    NoSamples,
    /// The corpus has more scenes than the report can count.
    #[error("benchmark corpus is too large")]
    TooManyScenes,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_alternates_and_covers_every_backend() {
        for seed in 0..8 {
            for repetition in 0..5 {
                let order = repetition_order(seed, repetition, 2);
                assert_ne!(order[0], repetition_order(seed, repetition + 1, 2)[0]);
                let mut sorted = order.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, vec![0, 1]);
            }
            let leaders = (0..3)
                .map(|repetition| repetition_order(seed, repetition, 3)[0])
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(leaders.len(), 3);
        }
        assert_eq!(repetition_order(3, 0, 1), vec![0]);
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        assert!((percentile(vec![4.0, 1.0, 3.0, 2.0], 95) - 4.0).abs() < f64::EPSILON);
        assert!((median(vec![4.0, 1.0, 3.0, 2.0]) - 3.0).abs() < f64::EPSILON);
        assert!(percentile(Vec::new(), 50).abs() < f64::EPSILON);
    }
}

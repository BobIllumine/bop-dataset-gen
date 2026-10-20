//! Versioned benchmark report and its generated Markdown rendering.

use std::fmt::Write as _;

use bop_core::BackendKind;
use bop_dataset::RunEnvironment;
use serde::Serialize;

use crate::{BENCHMARK_REPORT_SCHEMA_VERSION, BackendBenchmark, PhysicsBenchmark};

/// Host and source provenance for a benchmark run.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BenchmarkHost {
    /// `git rev-parse HEAD` of the working directory, when available.
    pub git_revision: Option<String>,
    /// Whether the working tree had uncommitted changes, when known.
    pub git_dirty: Option<bool>,
    /// CPU model name, when the platform reports one.
    pub cpu: Option<String>,
    /// Logical CPUs available to this process.
    pub logical_cpus: usize,
    /// Peak resident set size of this process, in bytes.
    pub peak_rss_bytes: Option<u64>,
    /// Peak resident set size of the largest finished child (renderer worker), in bytes.
    pub peak_child_rss_bytes: Option<u64>,
}

/// Asset preparation timings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PreparationBenchmark {
    /// First preparation in this process; cold only when the cache did not exist.
    pub first_seconds: f64,
    /// Immediate second preparation, which always reuses the cache.
    pub repeat_seconds: f64,
    /// Whether the convex-decomposition cache directory existed beforehand.
    pub cache_preexisting: bool,
}

/// Complete benchmark result, written as `report.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BenchmarkReport {
    /// Report layout version.
    pub schema_version: u16,
    /// Preset name (configuration file stem).
    pub preset: String,
    /// Fingerprint of the resolved configuration that produced the corpus.
    pub resolved_config_fingerprint: String,
    /// Scenes per pass.
    pub scene_count: u64,
    /// Cameras per scene.
    pub camera_count: usize,
    /// Measured repetitions per backend.
    pub repetitions: u32,
    /// Backend order of each repetition.
    pub order: Vec<Vec<BackendKind>>,
    /// Build and run environment.
    pub environment: RunEnvironment,
    /// Host and source provenance.
    pub host: BenchmarkHost,
    /// Asset preparation.
    pub preparation: PreparationBenchmark,
    /// Physics stage.
    pub physics: PhysicsBenchmark,
    /// One entry per backend, in command-line order.
    pub backends: Vec<BackendBenchmark>,
}

impl BenchmarkReport {
    /// Assembles a report with the current schema version and build environment.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        preset: String,
        resolved_config_fingerprint: String,
        camera_count: usize,
        order: Vec<Vec<BackendKind>>,
        host: BenchmarkHost,
        preparation: PreparationBenchmark,
        physics: PhysicsBenchmark,
        backends: Vec<BackendBenchmark>,
    ) -> Self {
        Self {
            schema_version: BENCHMARK_REPORT_SCHEMA_VERSION,
            preset,
            resolved_config_fingerprint,
            scene_count: physics.scene_count,
            camera_count,
            repetitions: u32::try_from(order.len()).unwrap_or(u32::MAX),
            order,
            environment: crate::current_run_environment(),
            host,
            preparation,
            physics,
            backends,
        }
    }

    /// Renders the report as Markdown. Every number comes from the JSON report.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut text = String::new();
        // Formatting into a String cannot fail.
        let _ = self.write_markdown(&mut text);
        text
    }

    fn write_markdown(&self, text: &mut String) -> std::fmt::Result {
        writeln!(text, "# Benchmark: {}\n", self.preset)?;
        writeln!(
            text,
            "{} scenes × {} camera(s), {} measured repetition(s) per backend.\n",
            self.scene_count, self.camera_count, self.repetitions
        )?;
        self.write_throughput(text)?;
        self.write_stages(text)?;
        self.write_repetitions(text)?;
        self.write_provenance(text)?;
        writeln!(
            text,
            "\nThese are measurements, not pass/fail thresholds. Compare backends only within one report."
        )
    }

    fn write_throughput(&self, text: &mut String) -> std::fmt::Result {
        writeln!(text, "## Throughput\n")?;
        writeln!(
            text,
            "| Backend | Device | Scenes/s | Frames/s | p50 scene | p95 scene | Render/scene | Readback/scene | Encode/scene | Output |"
        )?;
        writeln!(text, "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|")?;
        for backend in &self.backends {
            writeln!(
                text,
                "| {:?} | {} | {:.2} | {:.2} | {} | {} | {} | {} | {} | {} |",
                backend.backend.kind(),
                backend.backend.device_name().unwrap_or("-"),
                backend.scenes_per_second_median,
                backend.frames_per_second_median,
                milliseconds(backend.scene_latency_p50_seconds),
                milliseconds(backend.scene_latency_p95_seconds),
                milliseconds(backend.render_seconds_per_scene),
                milliseconds(backend.readback_seconds_per_scene),
                milliseconds(backend.encode_seconds_per_scene),
                mebibytes(backend.output_bytes),
            )?;
        }
        writeln!(
            text,
            "\nScene latency is the wall-clock render, encode, validate and commit of one scene. \
             Stage columns are renderer-reported medians per scene.\n"
        )
    }

    fn write_stages(&self, text: &mut String) -> std::fmt::Result {
        writeln!(text, "## Stages\n")?;
        let preparation = &self.preparation;
        writeln!(
            text,
            "- Asset preparation: first {} (cache {}), repeat {}",
            seconds(preparation.first_seconds),
            if preparation.cache_preexisting {
                "already existed"
            } else {
                "was cold"
            },
            seconds(preparation.repeat_seconds)
        )?;
        let physics = &self.physics;
        match (physics.wall_seconds, physics.scenes_per_second) {
            (Some(wall), Some(rate)) => writeln!(
                text,
                "- Physics: {} for {} scenes on {} worker(s), {rate:.2} scenes/s",
                seconds(wall),
                physics.scene_count,
                physics.workers
            )?,
            _ => writeln!(text, "- Physics: existing corpus reused, not timed")?,
        }
        writeln!(
            text,
            "- Physics steps per scene: {:.1}; rejection rate: {:.1}%",
            physics.steps_per_scene,
            physics.rejection_rate * 100.0
        )?;
        for backend in &self.backends {
            writeln!(
                text,
                "- {:?} prepare: {}; warm-up: {} scene(s)",
                backend.backend.kind(),
                seconds(backend.prepare_seconds),
                backend.warmup_scenes
            )?;
        }
        Ok(())
    }

    fn write_repetitions(&self, text: &mut String) -> std::fmt::Result {
        writeln!(text, "\n## Repetitions\n")?;
        writeln!(text, "| Repetition | Order | Backend | Wall | Scenes/s |")?;
        writeln!(text, "|---:|---:|---|---:|---:|")?;
        for backend in &self.backends {
            for sample in &backend.repetitions {
                writeln!(
                    text,
                    "| {} | {} | {:?} | {} | {:.2} |",
                    sample.repetition,
                    sample.order_position,
                    backend.backend.kind(),
                    seconds(sample.wall_seconds),
                    sample.scenes_per_second
                )?;
            }
        }
        Ok(())
    }

    fn write_provenance(&self, text: &mut String) -> std::fmt::Result {
        writeln!(text, "\n## Provenance\n")?;
        let host = &self.host;
        writeln!(
            text,
            "- Git: {} ({})",
            host.git_revision.as_deref().unwrap_or("unknown"),
            match host.git_dirty {
                Some(true) => "dirty",
                Some(false) => "clean",
                None => "state unknown",
            }
        )?;
        writeln!(
            text,
            "- Build: {}",
            serde_json::to_string(&self.environment).unwrap_or_default()
        )?;
        writeln!(
            text,
            "- CPU: {} ({} logical)",
            host.cpu.as_deref().unwrap_or("unknown"),
            host.logical_cpus
        )?;
        writeln!(
            text,
            "- Peak RSS: {} (bop-gen), {} (largest finished renderer worker)",
            host.peak_rss_bytes
                .map_or_else(|| "unknown".to_owned(), mebibytes),
            host.peak_child_rss_bytes
                .map_or_else(|| "-".to_owned(), mebibytes),
        )?;
        for backend in &self.backends {
            writeln!(
                text,
                "- {:?}: {} {}",
                backend.backend.kind(),
                backend.backend.implementation_version(),
                serde_json::to_string(backend.backend.properties()).unwrap_or_default()
            )?;
        }
        writeln!(
            text,
            "- Resolved config: `{}`",
            self.resolved_config_fingerprint
        )
    }
}

fn seconds(value: f64) -> String {
    format!("{value:.3} s")
}

fn milliseconds(value: f64) -> String {
    format!("{:.2} ms", value * 1000.0)
}

#[allow(clippy::cast_precision_loss)]
fn mebibytes(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

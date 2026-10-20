use std::time::Duration;

use bop_core::{BackendInfo, NonNegativeFinite};
use serde::Serialize;
use thiserror::Error;

/// Stable wall-clock summary for one renderer corpus and backend.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenderBenchmarkSummary {
    schema_version: u16,
    backend: BackendInfo,
    corpus: String,
    sample_count: u32,
    minimum_seconds: NonNegativeFinite,
    median_seconds: NonNegativeFinite,
    p95_seconds: NonNegativeFinite,
    maximum_seconds: NonNegativeFinite,
}

impl RenderBenchmarkSummary {
    /// Sorts nonempty wall-clock samples and computes nearest-rank statistics.
    ///
    /// # Errors
    ///
    /// Returns an error for blank corpus names, no samples, excessive counts,
    /// or durations that cannot be represented as finite `f32` seconds.
    pub fn try_new(
        backend: BackendInfo,
        corpus: impl Into<String>,
        mut samples: Vec<Duration>,
    ) -> Result<Self, InvalidRenderBenchmarkSummary> {
        let corpus = corpus.into();
        if corpus.trim().is_empty() {
            return Err(InvalidRenderBenchmarkSummary::BlankCorpus);
        }
        if samples.is_empty() {
            return Err(InvalidRenderBenchmarkSummary::EmptySamples);
        }
        samples.sort_unstable();
        let sample_count = u32::try_from(samples.len())
            .map_err(|_| InvalidRenderBenchmarkSummary::TooManySamples)?;
        let value = |index: usize| {
            NonNegativeFinite::try_new(samples[index].as_secs_f32())
                .map_err(|_| InvalidRenderBenchmarkSummary::DurationOverflow)
        };
        let median_index = samples.len() / 2;
        let p95_index = samples
            .len()
            .saturating_mul(95)
            .div_ceil(100)
            .saturating_sub(1);
        Ok(Self {
            schema_version: 1,
            backend,
            corpus,
            sample_count,
            minimum_seconds: value(0)?,
            median_seconds: value(median_index)?,
            p95_seconds: value(p95_index)?,
            maximum_seconds: value(samples.len() - 1)?,
        })
    }

    /// Returns the number of measured samples.
    #[must_use]
    pub const fn sample_count(&self) -> u32 {
        self.sample_count
    }
    /// Returns the minimum wall-clock duration in seconds.
    #[must_use]
    pub const fn minimum_seconds(&self) -> f32 {
        self.minimum_seconds.get()
    }
    /// Returns the upper median wall-clock duration in seconds.
    #[must_use]
    pub const fn median_seconds(&self) -> f32 {
        self.median_seconds.get()
    }
    /// Returns the nearest-rank p95 duration in seconds.
    #[must_use]
    pub const fn p95_seconds(&self) -> f32 {
        self.p95_seconds.get()
    }
    /// Returns the maximum wall-clock duration in seconds.
    #[must_use]
    pub const fn maximum_seconds(&self) -> f32 {
        self.maximum_seconds.get()
    }
}

/// Invalid renderer benchmark input.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidRenderBenchmarkSummary {
    /// Corpus identity is blank.
    #[error("benchmark corpus must not be blank")]
    BlankCorpus,
    /// No measured samples were supplied.
    #[error("benchmark requires at least one sample")]
    EmptySamples,
    /// Sample count exceeds the report representation.
    #[error("benchmark sample count exceeds u32")]
    TooManySamples,
    /// A duration exceeds finite `f32` seconds.
    #[error("benchmark duration exceeds finite f32 seconds")]
    DurationOverflow,
}

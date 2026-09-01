use std::num::NonZeroUsize;

use bpp_core::{RunPlan, SceneId, SceneSnapshot};
use thiserror::Error;

use crate::CancellationToken;

/// Worker-local physics engine used by the LEARN-02 executor.
pub trait PhysicsSimulator: Send {
    /// Generates one accepted renderer-neutral scene.
    ///
    /// # Errors
    ///
    /// Returns a fatal simulation error; retry policy remains inside the simulator.
    fn simulate(&mut self, scene_id: SceneId) -> Result<SceneSnapshot, PipelineError>;
}

/// Creates independent worker-local simulators without sharing a physics world.
pub trait SimulatorFactory: Sync {
    /// Concrete simulator owned by exactly one worker.
    type Simulator: PhysicsSimulator;

    /// Creates one isolated simulator.
    ///
    /// # Errors
    ///
    /// Returns a fatal worker-initialization error.
    fn create(&self) -> Result<Self::Simulator, PipelineError>;
}

/// Bounded parallel physics configuration reserved for LEARN-02.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParallelPhysicsExecutor {
    /// Number of worker-local simulators in the dedicated Rayon pool.
    pub workers: NonZeroUsize,
    /// Maximum accepted snapshots waiting for the downstream consumer.
    pub queue_capacity: NonZeroUsize,
}

impl ParallelPhysicsExecutor {
    /// Creates checked nonzero execution bounds.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidParallelPhysicsExecutor`] when either bound is zero.
    pub fn try_new(
        workers: usize,
        queue_capacity: usize,
    ) -> Result<Self, InvalidParallelPhysicsExecutor> {
        Ok(Self {
            workers: NonZeroUsize::new(workers)
                .ok_or(InvalidParallelPhysicsExecutor::ZeroWorkers)?,
            queue_capacity: NonZeroUsize::new(queue_capacity)
                .ok_or(InvalidParallelPhysicsExecutor::ZeroQueueCapacity)?,
        })
    }

    /// Runs bounded parallel physics generation.
    ///
    /// LEARN-02 owns this method body. See `docs/book/learn-02.md` for the
    /// scheduling protocol, failure rules, and verification commands.
    ///
    /// # Errors
    ///
    /// Returns the first fatal worker, simulation, panic, or receiver-drop error.
    pub fn run<F: SimulatorFactory>(
        &self,
        _plan: &RunPlan,
        _factory: F,
        _accepted: crossbeam_channel::Sender<SceneSnapshot>,
        _cancellation: &CancellationToken,
    ) -> Result<PhysicsStageReport, PipelineError> {
        todo!("LEARN-02: implement bounded Rayon physics scheduling")
    }
}

/// Invalid construction bounds for [`ParallelPhysicsExecutor`].
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidParallelPhysicsExecutor {
    /// No workers were requested.
    #[error("parallel physics worker count must be nonzero")]
    ZeroWorkers,
    /// The downstream queue cannot hold any snapshot.
    #[error("parallel physics queue capacity must be nonzero")]
    ZeroQueueCapacity,
}

/// Stable completion summary returned by the parallel physics stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicsStageReport {
    accepted_scene_count: u64,
    was_cancelled: bool,
}

impl PhysicsStageReport {
    /// Creates a stage report after every worker has joined.
    #[must_use]
    pub const fn new(accepted_scene_count: u64, was_cancelled: bool) -> Self {
        Self {
            accepted_scene_count,
            was_cancelled,
        }
    }

    /// Returns the number of snapshots successfully sent downstream.
    #[must_use]
    pub const fn accepted_scene_count(self) -> u64 {
        self.accepted_scene_count
    }

    /// Reports whether cooperative cancellation ended allocation.
    #[must_use]
    pub const fn was_cancelled(self) -> bool {
        self.was_cancelled
    }
}

/// Fatal failure propagated by the LEARN-02 physics stage.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PipelineError {
    /// A worker-local simulator failed.
    #[error("physics simulator failed: {0}")]
    Simulator(String),
    /// The downstream receiver disappeared before generation completed.
    #[error("accepted-scene receiver was dropped")]
    ReceiverDropped,
    /// A worker panicked; every remaining worker was still joined.
    #[error("physics worker panicked")]
    WorkerPanicked,
}

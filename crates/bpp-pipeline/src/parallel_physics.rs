use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use bpp_core::{AssetId, RunPlan, SceneId, SceneSnapshot};
use bpp_physics_rapier::{PreparedPhysicsAsset, SerialPhysicsGenerator};
use crossbeam_channel::{Receiver, Sender};
use thiserror::Error;

use crate::CancellationToken;

/// Worker-local physics engine used by the parallel executor.
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

/// Bounded parallel physics configuration.
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

    /// Reads the configured simulation worker count and queue capacity.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidParallelPhysicsExecutor`] when either configured bound is zero.
    pub fn from_plan(plan: &RunPlan) -> Result<Self, InvalidParallelPhysicsExecutor> {
        Self::try_new(
            plan.run().simulation_workers() as usize,
            plan.run().queue_capacity() as usize,
        )
    }

    /// Creates the bounded channel whose receiver consumes this executor's output.
    #[must_use]
    pub fn channel(self) -> (Sender<SceneSnapshot>, Receiver<SceneSnapshot>) {
        crossbeam_channel::bounded(self.queue_capacity.get())
    }

    /// Generates every scene of the run in parallel.
    ///
    /// See [`Self::run_scenes`] for the scheduling and failure rules.
    ///
    /// # Errors
    ///
    /// Returns the first fatal worker, simulation, panic, or receiver-drop error.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "callers hand over factory ownership; the pool only borrows it"
    )]
    pub fn run<F: SimulatorFactory>(
        &self,
        plan: &RunPlan,
        factory: F,
        accepted: Sender<SceneSnapshot>,
        cancellation: &CancellationToken,
    ) -> Result<PhysicsStageReport, PipelineError> {
        let scene_ids = (0..u64::from(plan.run().valid_scene_count()))
            .map(SceneId::new)
            .collect::<Vec<_>>();
        self.run_scenes(&scene_ids, &factory, accepted, cancellation)
    }

    /// Generates the given scenes on a dedicated pool of worker-local simulators.
    ///
    /// Workers claim scene IDs from a shared atomic cursor, so each ID is
    /// simulated at most once. Accepted snapshots go to `accepted` in completion
    /// order; a full channel blocks the sending worker, which bounds memory.
    /// The consumer must run on another thread, because this call returns only
    /// after every worker has joined.
    ///
    /// Cancellation is checked before claiming an ID and again before
    /// simulating it; a scene already being simulated may finish. The first
    /// fatal error, including a worker panic or a dropped receiver, cancels
    /// `cancellation` so the rest of the pipeline stops too.
    ///
    /// # Errors
    ///
    /// Returns the first fatal worker, simulation, panic, or receiver-drop error.
    pub fn run_scenes<F: SimulatorFactory>(
        &self,
        scene_ids: &[SceneId],
        factory: &F,
        accepted: Sender<SceneSnapshot>,
        cancellation: &CancellationToken,
    ) -> Result<PhysicsStageReport, PipelineError> {
        if cancellation.is_cancelled() {
            return Ok(PhysicsStageReport::new(0, true));
        }
        let workers = self.workers.get().min(scene_ids.len());
        if workers == 0 {
            return Ok(PhysicsStageReport::new(0, false));
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|index| format!("bpp-physics-{index}"))
            .build()
            .map_err(|error| PipelineError::ThreadPool(error.to_string()))?;

        let shared = SharedState {
            scene_ids,
            next: AtomicUsize::new(0),
            sent: AtomicU64::new(0),
            first_error: Mutex::new(None),
            cancellation,
        };
        pool.scope(|scope| {
            for _ in 0..workers {
                let sender = accepted.clone();
                let shared = &shared;
                scope.spawn(move |_| {
                    let outcome =
                        catch_unwind(AssertUnwindSafe(|| run_worker(factory, &sender, shared)));
                    match outcome {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => shared.fail(error),
                        Err(_) => shared.fail(PipelineError::WorkerPanicked),
                    }
                });
            }
            // Only worker clones remain, so the receiver disconnects as soon as
            // the last worker finishes.
            drop(accepted);
        });

        let first_error = shared
            .first_error
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        match first_error {
            Some(error) => Err(error),
            None => Ok(PhysicsStageReport::new(
                shared.sent.into_inner(),
                cancellation.is_cancelled(),
            )),
        }
    }
}

struct SharedState<'a> {
    scene_ids: &'a [SceneId],
    next: AtomicUsize,
    sent: AtomicU64,
    first_error: Mutex<Option<PipelineError>>,
    cancellation: &'a CancellationToken,
}

impl SharedState<'_> {
    fn fail(&self, error: PipelineError) {
        self.first_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(error);
        self.cancellation.cancel();
    }
}

fn run_worker<F: SimulatorFactory>(
    factory: &F,
    sender: &Sender<SceneSnapshot>,
    shared: &SharedState<'_>,
) -> Result<(), PipelineError> {
    let mut simulator = factory.create()?;
    loop {
        if shared.cancellation.is_cancelled() {
            return Ok(());
        }
        let index = shared.next.fetch_add(1, Ordering::Relaxed);
        let Some(&scene_id) = shared.scene_ids.get(index) else {
            return Ok(());
        };
        if shared.cancellation.is_cancelled() {
            return Ok(());
        }
        let snapshot = simulator.simulate(scene_id)?;
        sender
            .send(snapshot)
            .map_err(|_| PipelineError::ReceiverDropped)?;
        shared.sent.fetch_add(1, Ordering::Relaxed);
    }
}

/// Builds one Rapier generator per worker from a shared immutable collision catalog.
#[derive(Clone, Debug)]
pub struct RapierSimulatorFactory {
    plan: RunPlan,
    prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
}

impl RapierSimulatorFactory {
    /// Captures the plan and the `Arc`-shared prepared collision assets.
    #[must_use]
    pub const fn new(
        plan: RunPlan,
        prepared_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
    ) -> Self {
        Self {
            plan,
            prepared_assets,
        }
    }
}

impl SimulatorFactory for RapierSimulatorFactory {
    type Simulator = RapierSimulator;

    fn create(&self) -> Result<Self::Simulator, PipelineError> {
        SerialPhysicsGenerator::try_new(&self.plan, self.prepared_assets.clone())
            .map(RapierSimulator)
            .map_err(|error| PipelineError::Simulator(error.to_string()))
    }
}

/// One worker's private Rapier scene generator.
pub struct RapierSimulator(SerialPhysicsGenerator);

impl PhysicsSimulator for RapierSimulator {
    fn simulate(&mut self, scene_id: SceneId) -> Result<SceneSnapshot, PipelineError> {
        self.0
            .generate_scene(scene_id)
            .map_err(|error| PipelineError::Simulator(error.to_string()))
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

/// Fatal failure propagated by the parallel physics stage.
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
    /// The dedicated worker pool could not be created.
    #[error("physics worker pool could not start: {0}")]
    ThreadPool(String),
}

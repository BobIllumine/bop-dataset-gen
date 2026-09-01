//! LEARN-02 bounded parallel-physics exercise contract.

use std::{
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

use bpp_core::{RunConfig, SceneId, SceneSnapshot};
use bpp_pipeline::{
    CancellationToken, ParallelPhysicsExecutor, PhysicsSimulator, PipelineError, SimulatorFactory,
};

struct CountingFactory {
    created: AtomicUsize,
}

struct NeverCalledSimulator;

impl SimulatorFactory for &CountingFactory {
    type Simulator = NeverCalledSimulator;

    fn create(&self) -> Result<Self::Simulator, PipelineError> {
        self.created.fetch_add(1, Ordering::SeqCst);
        Ok(NeverCalledSimulator)
    }
}

impl PhysicsSimulator for NeverCalledSimulator {
    fn simulate(&mut self, _scene_id: SceneId) -> Result<SceneSnapshot, PipelineError> {
        Err(PipelineError::Simulator(
            "pre-cancelled work was allocated".to_owned(),
        ))
    }
}

#[test]
fn public_parallel_contract_has_checked_nonzero_bounds() {
    let executor = ParallelPhysicsExecutor::try_new(2, 3).unwrap();
    assert_eq!(executor.workers, NonZeroUsize::new(2).unwrap());
    assert_eq!(executor.queue_capacity, NonZeroUsize::new(3).unwrap());
    assert!(ParallelPhysicsExecutor::try_new(0, 1).is_err());
    assert!(ParallelPhysicsExecutor::try_new(1, 0).is_err());
}

#[test]
#[ignore = "LEARN-02: implement bounded Rayon physics scheduling"]
fn pre_cancelled_run_allocates_no_workers_or_scenes() {
    let plan = RunConfig::from_toml(include_str!("fixtures/serial_run.toml"))
        .unwrap()
        .into_plan();
    let executor = ParallelPhysicsExecutor::try_new(2, 1).unwrap();
    let factory = CountingFactory {
        created: AtomicUsize::new(0),
    };
    let (sender, receiver) = crossbeam_channel::bounded(1);
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let report = executor
        .run(&plan, &factory, sender, &cancellation)
        .unwrap();

    assert_eq!(report.accepted_scene_count(), 0);
    assert!(report.was_cancelled());
    assert_eq!(factory.created.load(Ordering::SeqCst), 0);
    assert!(receiver.is_empty());
}

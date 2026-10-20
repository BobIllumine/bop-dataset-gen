//! Scheduling, backpressure, failure, and determinism contract of the parallel physics executor.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use bop_assets::ConvexPart;
use bop_core::{AssetId, RunConfig, RunPlan, SceneId, SceneSnapshot};
use bop_physics_rapier::{PreparedPhysicsAsset, SerialPhysicsGenerator};
use bop_pipeline::{
    CancellationToken, ParallelPhysicsExecutor, PhysicsSimulator, PhysicsStageReport,
    PipelineError, RapierSimulatorFactory, SimulatorFactory,
};

fn snapshot(scene_id: SceneId) -> SceneSnapshot {
    serde_json::from_str(&format!(
        r#"{{"schema_version":1,"scene_id":{},"accepted_attempt":0,"seed_fingerprint":"seed","bin":{{"schema_version":1,"inner_dimensions_m":[1.0,1.0,1.0],"floor_thickness_m":0.1,"wall_thickness_m":0.1,"world_from_bin":{{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"material_label":"steel"}},"instances":[],"physics":{{"schema_version":1,"attempts":1,"steps":1,"simulated_seconds":0.1,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}}}}"#,
        scene_id.get()
    ))
    .unwrap()
}

fn scene_ids(count: u64) -> Vec<SceneId> {
    (0..count).map(SceneId::new).collect()
}

/// What a fake simulator does when it reaches `trigger`.
#[derive(Clone, Copy)]
enum Fault {
    None,
    Fail,
    Panic,
}

#[derive(Default)]
struct Counters {
    created: AtomicUsize,
    simulated: AtomicUsize,
}

struct FakeFactory {
    counters: Arc<Counters>,
    trigger: Option<SceneId>,
    fault: Fault,
    fail_create: bool,
}

impl FakeFactory {
    fn new() -> Self {
        Self::faulty(None, Fault::None)
    }

    fn faulty(trigger: Option<SceneId>, fault: Fault) -> Self {
        Self {
            counters: Arc::default(),
            trigger,
            fault,
            fail_create: false,
        }
    }
}

struct FakeSimulator {
    counters: Arc<Counters>,
    trigger: Option<SceneId>,
    fault: Fault,
}

impl SimulatorFactory for FakeFactory {
    type Simulator = FakeSimulator;

    fn create(&self) -> Result<Self::Simulator, PipelineError> {
        self.counters.created.fetch_add(1, Ordering::SeqCst);
        if self.fail_create {
            return Err(PipelineError::Simulator("cannot build world".to_owned()));
        }
        Ok(FakeSimulator {
            counters: Arc::clone(&self.counters),
            trigger: self.trigger,
            fault: self.fault,
        })
    }
}

impl PhysicsSimulator for FakeSimulator {
    fn simulate(&mut self, scene_id: SceneId) -> Result<SceneSnapshot, PipelineError> {
        self.counters.simulated.fetch_add(1, Ordering::SeqCst);
        if Some(scene_id) == self.trigger {
            match self.fault {
                Fault::None => {}
                Fault::Fail => {
                    return Err(PipelineError::Simulator(format!(
                        "scene {} exploded",
                        scene_id.get()
                    )));
                }
                Fault::Panic => panic!("injected simulator panic"),
            }
        }
        Ok(snapshot(scene_id))
    }
}

/// Runs the executor on a helper thread and collects everything it sends.
fn run_collecting(
    executor: ParallelPhysicsExecutor,
    ids: &[SceneId],
    factory: &FakeFactory,
    cancellation: &CancellationToken,
) -> (Result<PhysicsStageReport, PipelineError>, Vec<u64>) {
    let (sender, receiver) = executor.channel();
    thread::scope(|scope| {
        let physics = scope.spawn(|| executor.run_scenes(ids, factory, sender, cancellation));
        let mut seen = receiver
            .iter()
            .map(|scene| scene.scene_id().get())
            .collect::<Vec<_>>();
        seen.sort_unstable();
        (physics.join().unwrap(), seen)
    })
}

#[test]
fn every_scene_is_simulated_and_sent_exactly_once() {
    let executor = ParallelPhysicsExecutor::try_new(4, 2).unwrap();
    let factory = FakeFactory::new();
    let ids = scene_ids(64);

    let (result, seen) = run_collecting(executor, &ids, &factory, &CancellationToken::new());

    let report = result.unwrap();
    assert_eq!(report.accepted_scene_count(), 64);
    assert!(!report.was_cancelled());
    assert_eq!(seen, (0..64).collect::<Vec<_>>());
    assert_eq!(factory.counters.created.load(Ordering::SeqCst), 4);
    assert_eq!(factory.counters.simulated.load(Ordering::SeqCst), 64);
}

#[test]
fn run_covers_the_whole_plan_domain() {
    let plan = fixture_plan(5);
    let executor = ParallelPhysicsExecutor::try_new(3, 1).unwrap();
    let factory = FakeFactory::new();
    let (sender, receiver) = executor.channel();

    let report = thread::scope(|scope| {
        let physics =
            scope.spawn(|| executor.run(&plan, factory, sender, &CancellationToken::new()));
        assert_eq!(receiver.iter().count(), 5);
        physics.join().unwrap()
    })
    .unwrap();

    assert_eq!(report.accepted_scene_count(), 5);
}

#[test]
fn workers_never_outnumber_scenes() {
    let executor = ParallelPhysicsExecutor::try_new(8, 1).unwrap();
    let factory = FakeFactory::new();

    let (result, seen) =
        run_collecting(executor, &scene_ids(2), &factory, &CancellationToken::new());

    assert_eq!(result.unwrap().accepted_scene_count(), 2);
    assert_eq!(seen, vec![0, 1]);
    assert_eq!(factory.counters.created.load(Ordering::SeqCst), 2);
}

#[test]
fn an_empty_scene_list_starts_no_workers() {
    let executor = ParallelPhysicsExecutor::try_new(4, 1).unwrap();
    let factory = FakeFactory::new();

    let (result, seen) = run_collecting(executor, &[], &factory, &CancellationToken::new());

    assert_eq!(result.unwrap(), PhysicsStageReport::new(0, false));
    assert!(seen.is_empty());
    assert_eq!(factory.counters.created.load(Ordering::SeqCst), 0);
}

#[test]
fn pre_cancelled_run_allocates_no_workers_or_scenes() {
    let executor = ParallelPhysicsExecutor::try_new(2, 1).unwrap();
    let factory = FakeFactory::new();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let (result, seen) = run_collecting(executor, &scene_ids(10), &factory, &cancellation);

    assert_eq!(result.unwrap(), PhysicsStageReport::new(0, true));
    assert!(seen.is_empty());
    assert_eq!(factory.counters.created.load(Ordering::SeqCst), 0);
}

#[test]
fn a_slow_consumer_bounds_in_flight_snapshots() {
    let workers = 3;
    let capacity = 2;
    let executor = ParallelPhysicsExecutor::try_new(workers, capacity).unwrap();
    let factory = FakeFactory::new();
    let ids = scene_ids(40);
    let (sender, receiver) = executor.channel();

    thread::scope(|scope| {
        let physics =
            scope.spawn(|| executor.run_scenes(&ids, &factory, sender, &CancellationToken::new()));
        // Nobody reads yet: each worker can fill the queue and then hold one
        // more snapshot while blocked in `send`.
        thread::sleep(Duration::from_millis(200));
        let simulated = factory.counters.simulated.load(Ordering::SeqCst);
        assert!(
            simulated <= capacity + workers,
            "{simulated} scenes simulated with nobody consuming"
        );
        assert_eq!(receiver.len(), capacity);

        assert_eq!(receiver.iter().count(), 40);
        assert_eq!(physics.join().unwrap().unwrap().accepted_scene_count(), 40);
    });
}

#[test]
fn dropping_the_receiver_stops_every_worker() {
    let executor = ParallelPhysicsExecutor::try_new(3, 1).unwrap();
    let factory = FakeFactory::new();
    let ids = scene_ids(1_000);
    let cancellation = CancellationToken::new();
    let (sender, receiver) = executor.channel();

    let result = thread::scope(|scope| {
        let physics = scope.spawn(|| executor.run_scenes(&ids, &factory, sender, &cancellation));
        for _ in 0..2 {
            receiver.recv().unwrap();
        }
        drop(receiver);
        physics.join().unwrap()
    });

    assert!(matches!(result, Err(PipelineError::ReceiverDropped)));
    assert!(cancellation.is_cancelled());
    assert!(factory.counters.simulated.load(Ordering::SeqCst) < 1_000);
}

#[test]
fn a_simulator_error_is_returned_and_cancels_the_run() {
    let executor = ParallelPhysicsExecutor::try_new(4, 4).unwrap();
    let factory = FakeFactory::faulty(Some(SceneId::new(7)), Fault::Fail);
    let cancellation = CancellationToken::new();

    let (result, seen) = run_collecting(executor, &scene_ids(500), &factory, &cancellation);

    assert!(
        matches!(result, Err(PipelineError::Simulator(message)) if message == "scene 7 exploded")
    );
    assert!(cancellation.is_cancelled());
    assert!(!seen.contains(&7));
    assert!(factory.counters.simulated.load(Ordering::SeqCst) < 500);
}

#[test]
fn a_worker_panic_is_caught_and_every_worker_joins() {
    let executor = ParallelPhysicsExecutor::try_new(4, 4).unwrap();
    let factory = FakeFactory::faulty(Some(SceneId::new(3)), Fault::Panic);
    let cancellation = CancellationToken::new();

    let (result, seen) = run_collecting(executor, &scene_ids(500), &factory, &cancellation);

    assert!(matches!(result, Err(PipelineError::WorkerPanicked)));
    assert!(cancellation.is_cancelled());
    assert!(!seen.contains(&3));
}

#[test]
fn a_factory_error_is_returned() {
    let executor = ParallelPhysicsExecutor::try_new(2, 1).unwrap();
    let mut factory = FakeFactory::new();
    factory.fail_create = true;

    let (result, seen) = run_collecting(
        executor,
        &scene_ids(10),
        &factory,
        &CancellationToken::new(),
    );

    assert!(
        matches!(result, Err(PipelineError::Simulator(message)) if message == "cannot build world")
    );
    assert!(seen.is_empty());
}

#[test]
fn cancellation_stops_allocation_without_losing_sent_scenes() {
    let executor = ParallelPhysicsExecutor::try_new(3, 1).unwrap();
    let factory = FakeFactory::new();
    let ids = scene_ids(1_000);
    let cancellation = CancellationToken::new();
    let (sender, receiver) = executor.channel();

    let (report, mut seen) = thread::scope(|scope| {
        let physics = scope.spawn(|| executor.run_scenes(&ids, &factory, sender, &cancellation));
        let mut seen = Vec::new();
        for scene in &receiver {
            seen.push(scene.scene_id().get());
            if seen.len() == 3 {
                cancellation.cancel();
            }
        }
        (physics.join().unwrap().unwrap(), seen)
    });

    assert!(report.was_cancelled());
    assert_eq!(report.accepted_scene_count(), seen.len() as u64);
    assert!(seen.len() < 1_000);
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len() as u64, report.accepted_scene_count());
}

fn fixture_plan(scene_count: u32) -> RunPlan {
    let config = include_str!("fixtures/serial_run.toml")
        .replace(
            "valid_scene_count = 1",
            &format!("valid_scene_count = {scene_count}"),
        )
        .replace(
            "object_count = { kind = \"fixed\", value = 1 }",
            "object_count = { kind = \"fixed\", value = 2 }",
        );
    RunConfig::from_toml(&config).unwrap().into_plan()
}

fn prepared_cube() -> BTreeMap<AssetId, Arc<PreparedPhysicsAsset>> {
    let part = ConvexPart::try_new(
        vec![
            [-0.05, -0.05, -0.05],
            [0.05, -0.05, -0.05],
            [0.05, 0.05, -0.05],
            [-0.05, 0.05, -0.05],
            [-0.05, -0.05, 0.05],
            [0.05, -0.05, 0.05],
            [0.05, 0.05, 0.05],
            [-0.05, 0.05, 0.05],
        ],
        vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ],
    )
    .unwrap();
    BTreeMap::from([(
        AssetId::new("cube").unwrap(),
        Arc::new(PreparedPhysicsAsset::try_new(&[part]).unwrap()),
    )])
}

fn rapier_scenes(plan: &RunPlan, workers: usize) -> Vec<serde_json::Value> {
    let executor = ParallelPhysicsExecutor::try_new(workers, 2).unwrap();
    let factory = RapierSimulatorFactory::new(plan.clone(), prepared_cube());
    let (sender, receiver) = executor.channel();
    let mut scenes = thread::scope(|scope| {
        let physics =
            scope.spawn(|| executor.run(plan, factory, sender, &CancellationToken::new()));
        let scenes = receiver.iter().collect::<Vec<_>>();
        physics.join().unwrap().unwrap();
        scenes
    });
    scenes.sort_by_key(SceneSnapshot::scene_id);
    scenes
        .iter()
        .map(|scene| serde_json::to_value(scene).unwrap())
        .collect()
}

#[test]
fn rapier_output_does_not_depend_on_worker_count() {
    let plan = fixture_plan(6);
    let serial = SerialPhysicsGenerator::try_new(&plan, prepared_cube()).unwrap();
    let oracle = scene_ids(6)
        .into_iter()
        .map(|scene_id| serde_json::to_value(serial.generate_scene(scene_id).unwrap()).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(rapier_scenes(&plan, 1), oracle);
    assert_eq!(rapier_scenes(&plan, 4), oracle);
}

#[test]
fn rapier_failures_keep_their_typed_physics_error() {
    let plan = fixture_plan(1);
    let factory = RapierSimulatorFactory::new(plan, BTreeMap::new());
    assert!(matches!(factory.create(), Err(PipelineError::Physics(_))));
}

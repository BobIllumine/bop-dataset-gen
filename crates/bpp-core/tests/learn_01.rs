//! Executable contract for the LEARN-01 deterministic-planning exercise.

use std::iter::FusedIterator;

use bpp_core::{
    AttemptId, InvalidScenePlanError, SceneId, SceneJob, ScenePlanIter, SeedDeriver, SeedStage,
};

#[test]
fn scaffold_contracts_are_available_without_running_exercise_bodies() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_fused<T: FusedIterator>() {}

    assert_send_sync::<SeedDeriver>();
    assert_fused::<ScenePlanIter>();

    let _: fn(u64) -> SeedDeriver = SeedDeriver::new;
    let _: fn(&SeedDeriver, SceneId, AttemptId, SeedStage) -> [u8; 32] = SeedDeriver::derive;
    let _: fn(u64, u32) -> Result<ScenePlanIter, InvalidScenePlanError> = ScenePlanIter::try_new;

    assert_eq!(
        SeedStage::Population.protocol_label(),
        b"bpp.dataset.seed-stage.v1/population"
    );
    assert_eq!(
        SeedStage::Placement.protocol_label(),
        b"bpp.dataset.seed-stage.v1/placement"
    );
    assert_eq!(
        SeedStage::Camera.protocol_label(),
        b"bpp.dataset.seed-stage.v1/camera"
    );
    assert_eq!(
        SeedStage::Appearance.protocol_label(),
        b"bpp.dataset.seed-stage.v1/appearance"
    );

    let job = SceneJob::new(SceneId::new(12), AttemptId::new(3));
    assert_eq!(job.scene_id(), SceneId::new(12));
    assert_eq!(job.attempt_id(), AttemptId::new(3));

    assert_eq!(
        ScenePlanIter::try_new(0, 1).unwrap_err(),
        InvalidScenePlanError::ZeroRequestedSceneCount
    );
    assert_eq!(
        ScenePlanIter::try_new(1, 0).unwrap_err(),
        InvalidScenePlanError::ZeroAttemptLimit
    );
    assert!(ScenePlanIter::try_new(u64::MAX, u32::MAX).is_ok());
}

#[test]
#[ignore = "LEARN-01 exercise"]
fn learn_01_exercise_contracts() {
    seed_derivation_matches_stable_protocol_vectors();
    seed_derivation_is_repeatable_and_separates_every_tuple_component();
    seed_derivation_is_independent_of_thread_schedule();
    scene_plan_has_exact_scene_major_order_and_fused_exhaustion();
    scene_plan_iterators_are_independent();
    scene_plan_accepts_large_bounds_without_eager_length_arithmetic();
}

fn seed_derivation_matches_stable_protocol_vectors() {
    let vectors = [
        (
            0,
            SceneId::new(0),
            AttemptId::new(0),
            SeedStage::Population,
            [
                82, 204, 92, 242, 101, 160, 16, 33, 25, 172, 233, 102, 114, 181, 43, 3, 61, 25, 27,
                195, 35, 95, 93, 75, 119, 125, 42, 80, 79, 69, 23, 98,
            ],
        ),
        (
            0x0123_4567_89ab_cdef,
            SceneId::new(42),
            AttemptId::new(7),
            SeedStage::Placement,
            [
                246, 83, 33, 86, 17, 102, 86, 128, 59, 184, 221, 200, 167, 18, 229, 22, 31, 219,
                183, 250, 153, 30, 232, 65, 142, 186, 104, 210, 241, 161, 241, 200,
            ],
        ),
        (
            u64::MAX,
            SceneId::new(u64::MAX - 1),
            AttemptId::new(u32::MAX - 1),
            SeedStage::Appearance,
            [
                238, 187, 25, 17, 32, 226, 88, 122, 241, 111, 73, 195, 154, 161, 229, 250, 17, 108,
                81, 182, 10, 196, 90, 142, 190, 245, 36, 42, 30, 68, 149, 182,
            ],
        ),
    ];

    for (master_seed, scene, attempt, stage, expected) in vectors {
        assert_eq!(
            SeedDeriver::new(master_seed).derive(scene, attempt, stage),
            expected
        );
    }
}

fn seed_derivation_is_repeatable_and_separates_every_tuple_component() {
    let deriver = SeedDeriver::new(0x0123_4567_89ab_cdef);
    let scene = SceneId::new(42);
    let attempt = AttemptId::new(7);
    let baseline = deriver.derive(scene, attempt, SeedStage::Population);

    assert_eq!(
        deriver.derive(scene, attempt, SeedStage::Population),
        baseline
    );
    assert_ne!(
        deriver.derive(SceneId::new(43), attempt, SeedStage::Population),
        baseline
    );
    assert_ne!(
        deriver.derive(scene, AttemptId::new(8), SeedStage::Population),
        baseline
    );
    assert_ne!(
        deriver.derive(scene, attempt, SeedStage::Placement),
        baseline
    );
}

fn seed_derivation_is_independent_of_thread_schedule() {
    let deriver = SeedDeriver::new(91);
    let tuples = [
        (SceneId::new(0), AttemptId::new(0), SeedStage::Camera),
        (SceneId::new(5), AttemptId::new(2), SeedStage::Population),
        (SceneId::new(3), AttemptId::new(9), SeedStage::Appearance),
        (SceneId::new(1), AttemptId::new(4), SeedStage::Placement),
    ];
    let sequential = tuples.map(|(scene, attempt, stage)| deriver.derive(scene, attempt, stage));

    let threaded = std::thread::scope(|scope| {
        let mut handles = tuples
            .into_iter()
            .rev()
            .map(|(scene, attempt, stage)| {
                let deriver = &deriver;
                scope.spawn(move || deriver.derive(scene, attempt, stage))
            })
            .collect::<Vec<_>>();
        handles.reverse();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("seed worker must not panic"))
            .collect::<Vec<_>>()
    });

    assert_eq!(threaded, sequential);
}

fn scene_plan_has_exact_scene_major_order_and_fused_exhaustion() {
    let expected = [
        SceneJob::new(SceneId::new(0), AttemptId::new(0)),
        SceneJob::new(SceneId::new(0), AttemptId::new(1)),
        SceneJob::new(SceneId::new(0), AttemptId::new(2)),
        SceneJob::new(SceneId::new(1), AttemptId::new(0)),
        SceneJob::new(SceneId::new(1), AttemptId::new(1)),
        SceneJob::new(SceneId::new(1), AttemptId::new(2)),
    ];
    let mut plan = ScenePlanIter::try_new(2, 3).expect("nonzero bounds are valid");

    assert_eq!(plan.by_ref().collect::<Vec<_>>(), expected);
    assert_eq!(plan.next(), None);
    assert_eq!(plan.next(), None);
}

fn scene_plan_iterators_are_independent() {
    let mut first = ScenePlanIter::try_new(2, 2).expect("nonzero bounds are valid");
    let mut second = ScenePlanIter::try_new(2, 2).expect("nonzero bounds are valid");
    let first_job = SceneJob::new(SceneId::new(0), AttemptId::new(0));
    let second_job = SceneJob::new(SceneId::new(0), AttemptId::new(1));

    assert_eq!(first.next(), Some(first_job));
    assert_eq!(first.next(), Some(second_job));
    assert_eq!(second.next(), Some(first_job));
}

fn scene_plan_accepts_large_bounds_without_eager_length_arithmetic() {
    let mut plan = ScenePlanIter::try_new(u64::MAX, u32::MAX)
        .expect("the full identifier domains are valid bounds");

    assert_eq!(
        plan.next(),
        Some(SceneJob::new(SceneId::new(0), AttemptId::new(0)))
    );
    assert_eq!(
        plan.next(),
        Some(SceneJob::new(SceneId::new(0), AttemptId::new(1)))
    );
}

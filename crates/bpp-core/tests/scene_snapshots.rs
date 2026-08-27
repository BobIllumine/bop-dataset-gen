//! Behavioral tests for backend-neutral physical scene snapshots.

use bpp_core::{
    AssetId, AttemptId, BinSnapshot, InstanceId, InstanceSnapshot, InvalidBinSnapshotError,
    InvalidPhysicsReportError, InvalidResolvedPhysicalPropertiesError, InvalidSceneSnapshotError,
    PhysicsRejectionReason, PhysicsReport, PositiveMeters, RejectionCount,
    ResolvedPhysicalProperties, RigidTransform, SceneId, SceneSnapshot, SchemaVersion,
};

fn assert_same_float(actual: f32, expected: f32) {
    assert_eq!(actual.to_bits(), expected.to_bits());
}

#[test]
fn positive_metres_reject_invalid_scalars_at_construction_and_deserialization() {
    for invalid in [0.0, -0.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let error = PositiveMeters::try_new(invalid).unwrap_err();
        assert_eq!(error.value().to_bits(), invalid.to_bits());
    }

    let distance = PositiveMeters::try_new(2.5).expect("positive finite metres are valid");
    assert_same_float(distance.get(), 2.5);
    assert_eq!(serde_json::to_string(&distance).unwrap(), "2.5");
    assert_same_float(
        serde_json::from_str::<PositiveMeters>("2.5").unwrap().get(),
        2.5,
    );
    assert!(serde_json::from_str::<PositiveMeters>("0.0").is_err());
    assert!(serde_json::from_str::<PositiveMeters>("-1.0").is_err());
}

#[test]
fn physical_properties_report_the_invalid_field() {
    assert!(matches!(
        ResolvedPhysicalProperties::try_new(0.0, 0.5, 0.1),
        Err(InvalidResolvedPhysicalPropertiesError::InvalidMass { value })
            if value.to_bits() == 0.0_f32.to_bits()
    ));
    assert!(matches!(
        ResolvedPhysicalProperties::try_new(1.0, -0.1, 0.1),
        Err(InvalidResolvedPhysicalPropertiesError::InvalidFriction { value })
            if value.to_bits() == (-0.1_f32).to_bits()
    ));
    assert!(matches!(
        ResolvedPhysicalProperties::try_new(1.0, 0.5, 1.1),
        Err(InvalidResolvedPhysicalPropertiesError::InvalidRestitution { value })
            if value.to_bits() == 1.1_f32.to_bits()
    ));
    assert!(matches!(
        ResolvedPhysicalProperties::try_new(f32::NAN, 0.5, 0.1),
        Err(InvalidResolvedPhysicalPropertiesError::InvalidMass { value }) if value.is_nan()
    ));

    let properties = ResolvedPhysicalProperties::try_new(1.5, 0.6, 0.2).unwrap();
    assert_eq!(properties.schema_version(), SchemaVersion::current());
    assert_same_float(properties.mass_kg(), 1.5);
    assert_same_float(properties.friction(), 0.6);
    assert_same_float(properties.restitution(), 0.2);
}

#[test]
fn bin_snapshot_checks_each_dimension_and_preserves_its_transform() {
    assert!(matches!(
        BinSnapshot::try_new(
            [1.0, 0.0, 3.0],
            0.1,
            0.2,
            RigidTransform::identity(),
            "steel",
        ),
        Err(InvalidBinSnapshotError::InvalidInnerDimension { index: 1, .. })
    ));

    let bin = BinSnapshot::try_new(
        [1.0, 2.0, 3.0],
        0.1,
        0.2,
        RigidTransform::identity(),
        "steel",
    )
    .unwrap();
    assert_eq!(bin.schema_version(), SchemaVersion::current());
    for (actual, expected) in bin
        .inner_dimensions_m()
        .map(PositiveMeters::get)
        .into_iter()
        .zip([1.0, 2.0, 3.0])
    {
        assert_same_float(actual, expected);
    }
    assert_eq!(bin.world_from_bin(), RigidTransform::identity());
    assert_same_float(bin.floor_thickness_m().get(), 0.1);
    assert_same_float(bin.wall_thickness_m().get(), 0.2);
    assert_eq!(bin.material_label(), "steel");
}

#[test]
fn instance_snapshot_exposes_checked_domain_values() {
    let properties = ResolvedPhysicalProperties::try_new(1.5, 0.6, 0.2).unwrap();
    let instance = InstanceSnapshot::new(
        InstanceId::new(7).unwrap(),
        AssetId::new("asset-b").unwrap(),
        RigidTransform::identity(),
        properties,
    );

    assert_eq!(instance.schema_version(), SchemaVersion::current());
    assert_eq!(instance.instance_id().get(), 7);
    assert_eq!(instance.asset_id().as_str(), "asset-b");
    assert_eq!(instance.world_from_model(), RigidTransform::identity());
    assert_eq!(instance.physical_properties(), properties);
}

#[test]
fn physics_report_validates_numerics_and_canonicalizes_rejection_order() {
    let invalid_cases = [
        (0, 1.0, 0.1, 0.2),
        (1, f32::NAN, 0.1, 0.2),
        (1, -0.1, 0.1, 0.2),
        (1, 1.0, f32::INFINITY, 0.2),
        (1, 1.0, 0.1, -0.2),
    ];
    for (attempts, seconds, linear, angular) in invalid_cases {
        assert!(
            PhysicsReport::try_new(attempts, 50, seconds, linear, angular, 0.0, vec![]).is_err()
        );
    }

    assert!(matches!(
        PhysicsReport::try_new(
            2,
            50,
            1.0,
            0.1,
            0.2,
            0.0,
            vec![
                RejectionCount::new(PhysicsRejectionReason::Escape, 1),
                RejectionCount::new(PhysicsRejectionReason::Escape, 2),
            ],
        ),
        Err(InvalidPhysicsReportError::DuplicateRejectionReason {
            reason: PhysicsRejectionReason::Escape
        })
    ));

    let report = PhysicsReport::try_new(
        2,
        50,
        1.25,
        0.1,
        0.2,
        0.001,
        vec![
            RejectionCount::new(PhysicsRejectionReason::InvalidFinalState, 1),
            RejectionCount::new(PhysicsRejectionReason::SpawnSaturation, 3),
            RejectionCount::new(PhysicsRejectionReason::Timeout, 2),
        ],
    )
    .unwrap();

    assert_eq!(report.schema_version(), SchemaVersion::current());
    assert_eq!(report.attempts(), 2);
    assert_eq!(report.steps(), 50);
    assert_same_float(report.simulated_seconds(), 1.25);
    assert_same_float(report.final_max_linear_speed_mps(), 0.1);
    assert_same_float(report.final_max_angular_speed_rad_s(), 0.2);
    assert_same_float(report.final_max_penetration_m(), 0.001);
    assert_eq!(
        report
            .rejection_counts()
            .iter()
            .map(RejectionCount::reason)
            .collect::<Vec<_>>(),
        vec![
            PhysicsRejectionReason::SpawnSaturation,
            PhysicsRejectionReason::Timeout,
            PhysicsRejectionReason::InvalidFinalState,
        ]
    );
}

#[test]
fn scene_construction_sorts_instances_and_rejects_invalid_identity_inputs() {
    let bin = BinSnapshot::try_new(
        [1.0, 2.0, 3.0],
        0.1,
        0.2,
        RigidTransform::identity(),
        "steel",
    )
    .unwrap();
    let report = PhysicsReport::try_new(1, 50, 1.0, 0.1, 0.2, 0.0, vec![]).unwrap();
    let properties = ResolvedPhysicalProperties::try_new(1.5, 0.6, 0.2).unwrap();
    let instance = |id, asset| {
        InstanceSnapshot::new(
            InstanceId::new(id).unwrap(),
            AssetId::new(asset).unwrap(),
            RigidTransform::identity(),
            properties,
        )
    };

    assert!(matches!(
        SceneSnapshot::try_new(
            SceneId::new(9),
            AttemptId::new(2),
            "",
            bin.clone(),
            vec![],
            report.clone(),
        ),
        Err(InvalidSceneSnapshotError::EmptySeedFingerprint)
    ));
    assert!(matches!(
        SceneSnapshot::try_new(
            SceneId::new(9),
            AttemptId::new(2),
            "seed-abc",
            bin.clone(),
            vec![instance(2, "asset-b"), instance(2, "asset-c")],
            report.clone(),
        ),
        Err(InvalidSceneSnapshotError::DuplicateInstanceId { instance_id })
            if instance_id.get() == 2
    ));

    let scene = SceneSnapshot::try_new(
        SceneId::new(9),
        AttemptId::new(2),
        "seed-abc",
        bin.clone(),
        vec![instance(7, "asset-b"), instance(2, "asset-a")],
        report,
    )
    .unwrap();

    assert_eq!(scene.schema_version(), SchemaVersion::current());
    assert_eq!(scene.scene_id(), SceneId::new(9));
    assert_eq!(scene.accepted_attempt(), AttemptId::new(2));
    assert_eq!(scene.seed_fingerprint(), "seed-abc");
    assert_eq!(scene.bin(), &bin);
    assert_eq!(
        scene
            .instances()
            .iter()
            .map(|instance| instance.instance_id().get())
            .collect::<Vec<_>>(),
        vec![2, 7]
    );
    assert_eq!(scene.physics().attempts(), 1);
}

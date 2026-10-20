//! Literal JSON tests for versioned physical scene snapshots.

use bop_core::{
    AssetId, AttemptId, BinSnapshot, InstanceId, InstanceSnapshot, PhysicsRejectionReason,
    PhysicsReport, RejectionCount, ResolvedPhysicalProperties, RigidTransform, SceneId,
    SceneSnapshot,
};
use serde::de::DeserializeOwned;

const VALID_SCENE_JSON: &str = concat!(
    r#"{"schema_version":1,"scene_id":9,"accepted_attempt":2,"seed_fingerprint":"seed-abc","bins":[{"schema_version":1,"id":"bin","inner_dimensions_m":[1.0,2.0,3.0],"floor_thickness_m":0.1,"wall_thickness_m":0.2,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel","render_material":{"base_color_srgb":[0.6,0.6,0.62,1.0],"metallic":0.0,"roughness":0.6}}],"instances":["#,
    r#"{"schema_version":1,"instance_id":2,"asset_id":"asset-a","bin_id":"bin","world_from_model":{"translation":[2.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.5,"friction":0.6,"restitution":0.2}},"#,
    r#"{"schema_version":1,"instance_id":7,"asset_id":"asset-b","bin_id":"bin","world_from_model":{"translation":[7.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.5,"friction":0.6,"restitution":0.2}}],"#,
    r#""physics":{"schema_version":1,"attempts":22,"steps":50,"simulated_seconds":1.25,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.001,"rejection_counts":[{"schema_version":1,"reason":"spawn_saturation","count":1},{"schema_version":1,"reason":"non_finite_state","count":2},{"schema_version":1,"reason":"timeout","count":3},{"schema_version":1,"reason":"escape","count":4},{"schema_version":1,"reason":"excessive_penetration","count":5},{"schema_version":1,"reason":"invalid_final_state","count":6}]}}"#,
);

/// The same scene as written before scenes could hold several bins.
const LEGACY_SCENE_JSON: &str = concat!(
    r#"{"schema_version":1,"scene_id":9,"accepted_attempt":2,"seed_fingerprint":"seed-abc","bin":{"schema_version":1,"inner_dimensions_m":[1.0,2.0,3.0],"floor_thickness_m":0.1,"wall_thickness_m":0.2,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel","render_material":{"base_color_srgb":[0.6,0.6,0.62,1.0],"metallic":0.0,"roughness":0.6}},"instances":["#,
    r#"{"schema_version":1,"instance_id":2,"asset_id":"asset-a","world_from_model":{"translation":[2.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.5,"friction":0.6,"restitution":0.2}},"#,
    r#"{"schema_version":1,"instance_id":7,"asset_id":"asset-b","world_from_model":{"translation":[7.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.5,"friction":0.6,"restitution":0.2}}],"#,
    r#""physics":{"schema_version":1,"attempts":22,"steps":50,"simulated_seconds":1.25,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.001,"rejection_counts":[{"schema_version":1,"reason":"spawn_saturation","count":1},{"schema_version":1,"reason":"non_finite_state","count":2},{"schema_version":1,"reason":"timeout","count":3},{"schema_version":1,"reason":"escape","count":4},{"schema_version":1,"reason":"excessive_penetration","count":5},{"schema_version":1,"reason":"invalid_final_state","count":6}]}}"#,
);

fn valid_scene() -> SceneSnapshot {
    let properties = ResolvedPhysicalProperties::try_new(1.5, 0.6, 0.2).unwrap();
    let instance = |id, asset| {
        InstanceSnapshot::new(
            InstanceId::new(id).unwrap(),
            AssetId::new(asset).unwrap(),
            RigidTransform::try_new([f32::from(id), 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]).unwrap(),
            properties,
        )
    };
    let report = PhysicsReport::try_new(
        22,
        50,
        1.25,
        0.1,
        0.2,
        0.001,
        vec![
            RejectionCount::new(PhysicsRejectionReason::InvalidFinalState, 6),
            RejectionCount::new(PhysicsRejectionReason::ExcessivePenetration, 5),
            RejectionCount::new(PhysicsRejectionReason::Escape, 4),
            RejectionCount::new(PhysicsRejectionReason::Timeout, 3),
            RejectionCount::new(PhysicsRejectionReason::NonFiniteState, 2),
            RejectionCount::new(PhysicsRejectionReason::SpawnSaturation, 1),
        ],
    )
    .unwrap();

    SceneSnapshot::try_new(
        SceneId::new(9),
        AttemptId::new(2),
        "seed-abc",
        vec![
            BinSnapshot::try_new(
                [1.0, 2.0, 3.0],
                0.1,
                0.2,
                RigidTransform::identity(),
                "steel",
            )
            .unwrap(),
        ],
        vec![instance(7, "asset-b"), instance(2, "asset-a")],
        report,
    )
    .unwrap()
}

#[test]
fn valid_scene_json_is_schema_first_and_deterministically_ordered() {
    assert_eq!(
        serde_json::to_string(&valid_scene()).unwrap(),
        VALID_SCENE_JSON
    );
}

#[test]
fn valid_scene_json_round_trips_exactly() {
    let scene: SceneSnapshot = serde_json::from_str(VALID_SCENE_JSON).unwrap();

    assert_eq!(scene, valid_scene());
    assert_eq!(serde_json::to_string(&scene).unwrap(), VALID_SCENE_JSON);
}

#[test]
fn bin_json_without_a_render_material_reads_as_the_default_surface() {
    let legacy = VALID_SCENE_JSON.replace(
        r#","render_material":{"base_color_srgb":[0.6,0.6,0.62,1.0],"metallic":0.0,"roughness":0.6}"#,
        "",
    );
    assert_ne!(legacy, VALID_SCENE_JSON);
    let scene: SceneSnapshot = serde_json::from_str(&legacy).unwrap();

    assert_eq!(scene, valid_scene());
}

#[test]
fn physical_property_json_rejects_each_invalid_field() {
    for json in [
        r#"{"schema_version":1,"mass_kg":0.0,"friction":0.5,"restitution":0.1}"#,
        r#"{"schema_version":1,"mass_kg":1.0,"friction":-0.1,"restitution":0.1}"#,
        r#"{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":1.1}"#,
    ] {
        assert!(serde_json::from_str::<ResolvedPhysicalProperties>(json).is_err());
    }
}

#[test]
fn bin_json_rejects_nonpositive_dimensions() {
    for dimensions in ["[0.0,2.0,3.0]", "[1.0,-2.0,3.0]"] {
        let json = format!(
            r#"{{"schema_version":1,"inner_dimensions_m":{dimensions},"floor_thickness_m":0.1,"wall_thickness_m":0.2,"world_from_bin":{{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"material_label":"steel"}}"#
        );
        assert!(serde_json::from_str::<BinSnapshot>(&json).is_err());
    }
}

#[test]
fn physics_report_json_rejects_invalid_numerics_and_duplicate_reasons() {
    for json in [
        r#"{"schema_version":1,"attempts":0,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}"#,
        r#"{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":-1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}"#,
        r#"{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":-0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}"#,
        r#"{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":-0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}"#,
        r#"{"schema_version":1,"attempts":2,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[{"schema_version":1,"reason":"escape","count":1},{"schema_version":1,"reason":"escape","count":2}]}"#,
    ] {
        assert!(serde_json::from_str::<PhysicsReport>(json).is_err());
    }
}

#[test]
fn scene_json_rejects_empty_seed_duplicate_ids_and_unsorted_instances() {
    let instance_2 = r#"{"schema_version":1,"instance_id":2,"asset_id":"asset-a","world_from_model":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.1}}"#;
    let instance_7 = r#"{"schema_version":1,"instance_id":7,"asset_id":"asset-b","world_from_model":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.1}}"#;
    let scene = |seed: &str, instances: &str| {
        format!(
            r#"{{"schema_version":1,"scene_id":9,"accepted_attempt":2,"seed_fingerprint":"{seed}","bin":{{"schema_version":1,"inner_dimensions_m":[1.0,2.0,3.0],"floor_thickness_m":0.1,"wall_thickness_m":0.2,"world_from_bin":{{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"material_label":"steel"}},"instances":[{instances}],"physics":{{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}}}}"#
        )
    };

    assert!(serde_json::from_str::<SceneSnapshot>(&scene("", instance_2)).is_err());
    assert!(
        serde_json::from_str::<SceneSnapshot>(&scene(
            "seed-abc",
            &format!("{instance_2},{instance_2}")
        ))
        .is_err()
    );
    assert!(
        serde_json::from_str::<SceneSnapshot>(&scene(
            "seed-abc",
            &format!("{instance_7},{instance_2}")
        ))
        .is_err()
    );
}

#[test]
fn struct_json_rejects_unknown_fields() {
    assert_rejected::<ResolvedPhysicalProperties>(
        r#"{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.1,"extra":true}"#,
    );
    assert_rejected::<BinSnapshot>(
        r#"{"schema_version":1,"inner_dimensions_m":[1.0,2.0,3.0],"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"extra":true}"#,
    );
    assert_rejected::<InstanceSnapshot>(
        r#"{"schema_version":1,"instance_id":2,"asset_id":"asset-a","world_from_model":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.1},"extra":true}"#,
    );
    assert_rejected::<RejectionCount>(
        r#"{"schema_version":1,"reason":"timeout","count":2,"extra":true}"#,
    );
    assert_rejected::<PhysicsReport>(
        r#"{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"rejection_counts":[],"extra":true}"#,
    );
    assert_rejected::<SceneSnapshot>(
        r#"{"schema_version":1,"scene_id":9,"accepted_attempt":2,"seed_fingerprint":"seed-abc","bin":{"schema_version":1,"inner_dimensions_m":[1.0,2.0,3.0],"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"rejection_counts":[]},"extra":true}"#,
    );
}

#[test]
fn scene_json_rejects_an_unsupported_nested_schema_version() {
    let json = r#"{"schema_version":1,"scene_id":9,"accepted_attempt":2,"seed_fingerprint":"seed-abc","bin":{"schema_version":1,"inner_dimensions_m":[1.0,2.0,3.0],"floor_thickness_m":0.1,"wall_thickness_m":0.2,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[{"schema_version":1,"instance_id":2,"asset_id":"asset-a","world_from_model":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":2,"mass_kg":1.0,"friction":0.5,"restitution":0.1}}],"physics":{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}}"#;

    assert!(serde_json::from_str::<SceneSnapshot>(json).is_err());
}

fn assert_rejected<T: DeserializeOwned>(json: &str) {
    assert!(serde_json::from_str::<T>(json).is_err());
}

#[test]
fn single_bin_scene_json_written_before_bin_lists_reads_as_the_same_scene() {
    let scene: SceneSnapshot = serde_json::from_str(LEGACY_SCENE_JSON).unwrap();

    assert_eq!(scene, valid_scene());
    assert_eq!(scene.bins()[0].id(), bop_core::DEFAULT_BIN_ID);
}

#[test]
fn instances_must_name_one_of_the_scene_bins_and_bin_ids_are_unique() {
    let unknown = VALID_SCENE_JSON.replacen(r#""bin_id":"bin""#, r#""bin_id":"left""#, 1);
    let error = serde_json::from_str::<SceneSnapshot>(&unknown).unwrap_err();
    assert!(error.to_string().contains("unknown bin"), "{error}");

    let bin = &VALID_SCENE_JSON[VALID_SCENE_JSON
        .find(r#"{"schema_version":1,"id":"bin""#)
        .unwrap()
        ..VALID_SCENE_JSON.find(r#"],"instances""#).unwrap()];
    let duplicated = VALID_SCENE_JSON.replacen(bin, &format!("{bin},{bin}"), 1);
    let error = serde_json::from_str::<SceneSnapshot>(&duplicated).unwrap_err();
    assert!(error.to_string().contains("duplicate bin ID"), "{error}");

    let both = VALID_SCENE_JSON.replacen(r#""bins":["#, &format!(r#""bin":{bin},"bins":["#), 1);
    assert!(serde_json::from_str::<SceneSnapshot>(&both).is_err());
}

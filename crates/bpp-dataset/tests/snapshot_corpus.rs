//! Atomic simulation-corpus handoff contracts.

use std::fs;

use bpp_core::{SceneId, SceneSnapshot};
use bpp_dataset::{SnapshotCorpusError, read_snapshot_corpus, write_scene_snapshot};

const SCENE: &str = r#"{"schema_version":1,"scene_id":7,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,0.8,0.5],"floor_thickness_m":0.02,"wall_thickness_m":0.03,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":16,"simulated_seconds":0.1,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}}"#;

fn scene(id: u64) -> SceneSnapshot {
    let mut value: serde_json::Value = serde_json::from_str(SCENE).unwrap();
    value["scene_id"] = serde_json::json!(id);
    serde_json::from_value(value).unwrap()
}

#[test]
fn snapshots_commit_atomically_and_read_in_scene_id_order() {
    let root = tempfile::tempdir().unwrap();
    write_scene_snapshot(root.path(), &scene(7)).unwrap();
    write_scene_snapshot(root.path(), &scene(2)).unwrap();

    let snapshots = read_snapshot_corpus(root.path()).unwrap();
    assert_eq!(
        snapshots
            .iter()
            .map(SceneSnapshot::scene_id)
            .collect::<Vec<_>>(),
        vec![SceneId::new(2), SceneId::new(7)]
    );
    assert_eq!(
        serde_json::from_slice::<SceneSnapshot>(
            &fs::read(root.path().join("scenes/00000000000000000007/scene.json")).unwrap()
        )
        .unwrap(),
        scene(7)
    );
    assert!(
        root.path()
            .join(".staging")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn existing_or_malformed_snapshot_output_is_never_silently_accepted() {
    let root = tempfile::tempdir().unwrap();
    write_scene_snapshot(root.path(), &scene(7)).unwrap();
    assert!(matches!(
        write_scene_snapshot(root.path(), &scene(7)),
        Err(SnapshotCorpusError::DestinationExists { .. })
    ));

    fs::create_dir_all(root.path().join("scenes/not-a-scene")).unwrap();
    assert!(matches!(
        read_snapshot_corpus(root.path()),
        Err(SnapshotCorpusError::InvalidSceneDirectory { .. })
    ));
}

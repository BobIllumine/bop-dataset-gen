//! Public isolated scene-staging contracts.

use std::fs;

use bop_core::{CameraId, SceneId};
use bop_dataset::SceneStagingArea;

#[test]
fn staging_paths_are_isolated_canonical_and_removed_when_abandoned() {
    let root = tempfile::tempdir().expect("temporary dataset root");
    let first = SceneStagingArea::create(root.path(), SceneId::new(42)).expect("staging creates");
    let second = SceneStagingArea::create(root.path(), SceneId::new(42)).expect("staging isolates");

    assert_ne!(first.root(), second.root());
    assert!(first.root().starts_with(root.path().join(".staging")));
    assert_eq!(first.scene_id(), SceneId::new(42));
    assert_eq!(first.scene_metadata_path(), first.root().join("scene.json"));
    let camera = first
        .prepare_camera(CameraId::new(7))
        .expect("camera directory creates");
    assert_eq!(camera, first.root().join("frames/00007"));
    assert_eq!(
        first.frame_metadata_path(CameraId::new(7)),
        camera.join("frame.json")
    );
    assert_eq!(first.rgb_path(CameraId::new(7)), camera.join("rgb.png"));
    assert_eq!(first.depth_path(CameraId::new(7)), camera.join("depth.exr"));
    assert_eq!(
        first.instance_path(CameraId::new(7)),
        camera.join("instance.png")
    );

    let abandoned = first.root().to_owned();
    drop(first);
    assert!(!abandoned.exists());
    assert!(second.root().exists());
}

#[test]
fn existing_camera_directory_is_idempotent_but_file_collision_is_typed_io() {
    let root = tempfile::tempdir().expect("temporary dataset root");
    let staging = SceneStagingArea::create(root.path(), SceneId::new(1)).expect("staging creates");
    let camera_id = CameraId::new(2);
    let first = staging.prepare_camera(camera_id).expect("first create");
    assert_eq!(staging.prepare_camera(camera_id).unwrap(), first);

    let blocked = staging.camera_dir(CameraId::new(3));
    fs::write(&blocked, b"not a directory").expect("collision fixture writes");
    assert!(staging.prepare_camera(CameraId::new(3)).is_err());
}

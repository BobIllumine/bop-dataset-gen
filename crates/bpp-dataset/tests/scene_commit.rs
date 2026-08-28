//! Atomic scene-publication contracts.

use std::{fs, path::Path};

use bpp_core::{CameraId, RenderChannels, SceneId};
use bpp_dataset::{SceneArtifactRequirements, SceneCommitError, SceneStagingArea};

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn complete_scene_is_renamed_once_into_its_canonical_destination() {
    let root = tempfile::tempdir().unwrap();
    let staging = SceneStagingArea::create(root.path(), SceneId::new(42)).unwrap();
    let staged_root = staging.root().to_owned();
    let camera = CameraId::new(7);
    write(&staging.scene_metadata_path(), b"{}");
    write(&staging.frame_metadata_path(camera), b"{}");
    write(&staging.rgb_path(camera), b"rgb");
    write(&staging.depth_path(camera), b"depth");
    let requirements = SceneArtifactRequirements::try_new(
        vec![camera],
        RenderChannels::try_new(true, true, false).unwrap(),
    )
    .unwrap();

    let committed = staging.commit(&requirements).unwrap();

    assert_eq!(committed, root.path().join("scenes/00000000000000000042"));
    assert!(!staged_root.exists());
    assert_eq!(
        fs::read(committed.join("frames/00007/rgb.png")).unwrap(),
        b"rgb"
    );
    assert_eq!(
        fs::read(committed.join("frames/00007/depth.exr")).unwrap(),
        b"depth"
    );
}

#[test]
fn missing_or_empty_required_artifact_prevents_publication() {
    let root = tempfile::tempdir().unwrap();
    let camera = CameraId::new(1);
    let requirements = SceneArtifactRequirements::try_new(
        vec![camera],
        RenderChannels::try_new(true, false, false).unwrap(),
    )
    .unwrap();
    let staging = SceneStagingArea::create(root.path(), SceneId::new(3)).unwrap();
    write(&staging.scene_metadata_path(), b"{}");
    write(&staging.frame_metadata_path(camera), b"{}");

    assert_eq!(
        staging.commit(&requirements).unwrap_err(),
        SceneCommitError::MissingArtifact {
            relative_path: "frames/00001/rgb.png".into(),
        }
    );
    assert!(!root.path().join("scenes/00000000000000000003").exists());

    let staging = SceneStagingArea::create(root.path(), SceneId::new(4)).unwrap();
    write(&staging.scene_metadata_path(), b"{}");
    write(&staging.frame_metadata_path(camera), b"{}");
    write(&staging.rgb_path(camera), b"");
    assert_eq!(
        staging.commit(&requirements).unwrap_err(),
        SceneCommitError::EmptyArtifact {
            relative_path: "frames/00001/rgb.png".into(),
        }
    );
}

#[test]
fn existing_scene_is_never_replaced() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("scenes/00000000000000000005");
    write(&destination.join("sentinel"), b"keep");
    let staging = SceneStagingArea::create(root.path(), SceneId::new(5)).unwrap();
    let camera = CameraId::new(0);
    write(&staging.scene_metadata_path(), b"{}");
    write(&staging.frame_metadata_path(camera), b"{}");
    write(&staging.instance_path(camera), b"instance");
    let requirements = SceneArtifactRequirements::try_new(
        vec![camera],
        RenderChannels::try_new(false, false, true).unwrap(),
    )
    .unwrap();

    assert_eq!(
        staging.commit(&requirements).unwrap_err(),
        SceneCommitError::DestinationExists {
            scene_id: SceneId::new(5)
        }
    );
    assert_eq!(fs::read(destination.join("sentinel")).unwrap(), b"keep");
}

#[test]
fn requirements_reject_empty_and_duplicate_camera_sets() {
    let channels = RenderChannels::try_new(true, false, false).unwrap();
    assert!(SceneArtifactRequirements::try_new(vec![], channels).is_err());
    assert!(
        SceneArtifactRequirements::try_new(vec![CameraId::new(2), CameraId::new(2)], channels)
            .is_err()
    );
}

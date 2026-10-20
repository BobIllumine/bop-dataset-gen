//! Deep staged-frame validation before atomic publication.

use bop_core::RenderSceneRequest;
use bop_dataset::{
    DatasetValidationError, InstanceFrame, SceneStagingArea, validate_and_write_checksums,
};

const REQUEST: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":3,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,1.0,1.0],"floor_thickness_m":0.1,"wall_thickness_m":0.1,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":1,"simulated_seconds":0.01,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":0,"image_size":{"width":1,"height":1},"k":[1.0,0.0,0.5,0.0,1.0,0.5,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.0,0.0,0.0,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":1.0},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":false,"depth":false,"instance":true}}"#;

#[test]
fn undeclared_visible_instance_id_is_rejected_before_checksum_publication() {
    let request: RenderSceneRequest = serde_json::from_str(REQUEST).unwrap();
    let root = tempfile::tempdir().unwrap();
    let staging = SceneStagingArea::create(root.path(), request.scene().scene_id()).unwrap();
    staging
        .prepare_camera(request.cameras()[0].camera_id())
        .unwrap();
    InstanceFrame::try_new(1, 1, vec![7])
        .unwrap()
        .write_png(
            &staging.instance_path(request.cameras()[0].camera_id()),
            bop_core::PngCompression::Fast,
        )
        .unwrap();

    assert!(matches!(
        validate_and_write_checksums(&staging, &request),
        Err(DatasetValidationError::IllegalInstanceId {
            instance_id: 7,
            index: 0,
            ..
        })
    ));
    assert!(!staging.root().join("checksums.json").exists());
}

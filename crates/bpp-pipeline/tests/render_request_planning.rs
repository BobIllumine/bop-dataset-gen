//! Deterministic compilation from checked run plans to renderer requests.

use bpp_core::{CameraExtrinsics, CameraPose, RunConfig, SceneSnapshot};
use bpp_pipeline::RenderRequestPlanner;

const PLAN: &str = include_str!("fixtures/serial_run.toml");
const SCENE: &str = r#"{"schema_version":1,"scene_id":7,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,0.8,0.5],"floor_thickness_m":0.02,"wall_thickness_m":0.03,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":16,"simulated_seconds":0.1,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}}"#;

fn plan(input: &str) -> bpp_core::RunPlan {
    RunConfig::from_toml(input).unwrap().into_plan()
}

fn scene() -> SceneSnapshot {
    serde_json::from_str(SCENE).unwrap()
}

#[test]
fn fixed_camera_and_vertical_fov_compile_to_the_canonical_request() {
    let plan = plan(PLAN);
    let planner = RenderRequestPlanner::new(&plan);
    let request = planner.plan(scene()).unwrap();
    let camera = request.cameras()[0];

    assert_eq!(camera.camera_id().get(), 1);
    assert_eq!(camera.image_size().width(), 16);
    assert_eq!(camera.image_size().height(), 12);
    let expected_focal = 12.0 / (2.0 * (30.0_f32.to_radians()).tan());
    assert!((camera.k().fx() - expected_focal).abs() <= f32::EPSILON);
    assert!((camera.k().fy() - expected_focal).abs() <= f32::EPSILON);
    assert_eq!(camera.k().cx().to_bits(), 8.0_f32.to_bits());
    assert_eq!(camera.k().cy().to_bits(), 6.0_f32.to_bits());
    let CameraPose::FixedWorldFromCamera(world_from_camera) = plan.cameras()[0].pose() else {
        panic!("fixture camera must be fixed");
    };
    assert_eq!(
        camera.camera_from_world(),
        CameraExtrinsics::try_from_world_from_z_up_camera(world_from_camera).unwrap()
    );
    assert_eq!(request.appearance(), plan.appearance().snapshot());
    assert_eq!(request.required_channels(), plan.output_channels());
}

#[test]
fn shell_camera_replays_exactly_and_changes_with_scene_identity() {
    let shell = PLAN.replace(
        "pose = { fixed_world_from_camera = { translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] } }",
        "pose = { shell = { min_radius_m = 0.8, max_radius_m = 1.2, min_azimuth_deg = -180.0, max_azimuth_deg = 180.0, min_elevation_deg = 20.0, max_elevation_deg = 70.0, target_bin_m = [0.0, 0.0, 0.2] } }",
    );
    let plan = plan(&shell);
    let planner = RenderRequestPlanner::new(&plan);
    let first = planner.plan(scene()).unwrap();
    let replay = planner.plan(scene()).unwrap();
    let mut other_json: serde_json::Value = serde_json::from_str(SCENE).unwrap();
    other_json["scene_id"] = serde_json::json!(8);
    let other = planner
        .plan(serde_json::from_value(other_json).unwrap())
        .unwrap();

    assert_eq!(
        serde_json::to_string(&first.cameras()[0]).unwrap(),
        r#"{"schema_version":1,"camera_id":1,"image_size":{"width":16,"height":12},"k":[10.392305,0.0,8.0,0.0,10.392305,6.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[-8.940696e-8,0.115130655,1.1363792],"rotation_xyzw":[0.74736434,-0.59185594,0.18743753,0.23668616]}},"near_m":0.01,"far_m":10.0}"#
    );
    assert_eq!(first.cameras(), replay.cameras());
    assert_ne!(first.cameras(), other.cameras());
}

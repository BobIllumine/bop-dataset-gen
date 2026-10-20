//! Deterministic compilation from checked run plans to renderer requests.

use bop_core::{CameraExtrinsics, CameraPose, MaterialScope, RunConfig, SceneSnapshot};
use bop_pipeline::RenderRequestPlanner;

const PLAN: &str = include_str!("fixtures/serial_run.toml");
const SCENE: &str = r#"{"schema_version":1,"scene_id":7,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,0.8,0.5],"floor_thickness_m":0.02,"wall_thickness_m":0.03,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":16,"simulated_seconds":0.1,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}}"#;

fn plan(input: &str) -> bop_core::RunPlan {
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

fn scene_with_cubes(count: u16) -> SceneSnapshot {
    let mut json: serde_json::Value = serde_json::from_str(SCENE).unwrap();
    json["instances"] = (1..=count)
        .map(|id| {
            serde_json::json!({
                "schema_version": 1, "instance_id": id, "asset_id": "cube",
                "world_from_model": {"translation": [0.0, 0.0, 0.1 * f64::from(id)], "rotation_xyzw": [0.0, 0.0, 0.0, 1.0]},
                "physical_properties": {"schema_version": 1, "mass_kg": 0.5, "friction": 0.5, "restitution": 0.1}
            })
        })
        .collect();
    serde_json::from_value(json).unwrap()
}

#[test]
fn material_defaults_cover_primitives_without_a_file_material() {
    let plan = plan(PLAN);
    let request = RenderRequestPlanner::new(&plan)
        .plan(scene_with_cubes(2))
        .unwrap();

    let materials = request.instance_materials();
    assert_eq!(materials.len(), 2);
    for material in materials {
        assert_eq!(material.scope(), MaterialScope::Unassigned);
        assert_eq!(
            material.base_color_srgb().rgba().map(f32::to_bits),
            [0.5_f32, 0.5, 0.5, 1.0].map(f32::to_bits)
        );
        assert_eq!(material.metallic().to_bits(), 0.0_f32.to_bits());
        assert_eq!(material.roughness().to_bits(), 0.5_f32.to_bits());
    }
}

#[test]
fn visual_overrides_replace_file_materials_with_replayable_per_instance_samples() {
    let overridden = PLAN.replace(
        "vhacd = { resolution = 1000",
        "visual_override = { base_color_srgb = [0.8, 0.1, 0.1, 1.0], metallic = { kind = \"choice\", values = [{ value = 0.0, weight = 1.0 }, { value = 1.0, weight = 1.0 }] }, roughness = { kind = \"uniform\", min = 0.2, max = 0.8 } }\nvhacd = { resolution = 1000",
    );
    let plan = plan(&overridden);
    let planner = RenderRequestPlanner::new(&plan);
    let request = planner.plan(scene_with_cubes(8)).unwrap();
    let replay = planner.plan(scene_with_cubes(8)).unwrap();

    let materials = request.instance_materials();
    assert_eq!(materials, replay.instance_materials());
    assert!(materials.iter().all(|material| {
        material.scope() == MaterialScope::All
            && material.base_color_srgb().rgba().map(f32::to_bits)
                == [0.8_f32, 0.1, 0.1, 1.0].map(f32::to_bits)
            && (0.2..0.8).contains(&material.roughness())
            && [0.0, 1.0].contains(&material.metallic())
    }));
    let roughness: std::collections::BTreeSet<u32> = materials
        .iter()
        .map(|material| material.roughness().to_bits())
        .collect();
    assert_eq!(
        roughness.len(),
        8,
        "every instance samples its own roughness"
    );
    let metals = materials
        .iter()
        .filter(|material| material.metallic() > 0.5)
        .count();
    assert!((1..8).contains(&metals), "both metallic choices occur");

    // The materials travel with the request and are checked on the way back.
    let json = serde_json::to_string(&request).unwrap();
    let parsed: bop_core::RenderSceneRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.instance_materials(), materials);
    let mut stray: serde_json::Value = serde_json::from_str(&json).unwrap();
    stray["instance_materials"][0]["instance_id"] = serde_json::json!(99);
    assert!(serde_json::from_value::<bop_core::RenderSceneRequest>(stray).is_err());
}

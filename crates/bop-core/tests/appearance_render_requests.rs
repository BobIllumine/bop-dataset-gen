//! Literal contract tests for appearance and render-scene request DTOs.

use bop_core::{
    AmbientLight, AppearanceSnapshot, CameraId, CameraSnapshot, DirectionalLight,
    InvalidDirectionalLightError, InvalidLinearRgbError, InvalidNonNegativeFiniteError,
    InvalidRenderSceneRequestError, InvalidSrgbColorError, LinearRgb, Meters, NonNegativeFinite,
    PointLight, RenderChannels, RenderSceneRequest, SceneSnapshot, SchemaVersion, SrgbColor,
};

const VALID_APPEARANCE_JSON: &str = concat!(
    r#"{"schema_version":1,"background_srgb":[0.1,0.2,0.3,1.0],"ambient_light":{"schema_version":1,"color":[0.4,0.5,0.6],"intensity":0.25},"directional_lights":["#,
    r#"{"schema_version":1,"direction_world":[1.0,0.0,0.0],"color":[1.0,2.0,3.0],"illuminance_lux":100.0},"#,
    r#"{"schema_version":1,"direction_world":[0.0,-1.0,0.0],"color":[4.0,5.0,6.0],"illuminance_lux":200.0}],"point_lights":["#,
    r#"{"schema_version":1,"position_world_m":[3.0,2.0,1.0],"color":[7.0,8.0,9.0],"luminous_power_lm":300.0},"#,
    r#"{"schema_version":1,"position_world_m":[-3.0,-2.0,-1.0],"color":[10.0,11.0,12.0],"luminous_power_lm":400.0}]}"#,
);

const VALID_SCENE_JSON: &str = r#"{"schema_version":1,"scene_id":9,"accepted_attempt":2,"seed_fingerprint":"seed-abc","bin":{"schema_version":1,"inner_dimensions_m":[1.0,2.0,3.0],"floor_thickness_m":0.1,"wall_thickness_m":0.2,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}}"#;

const CAMERA_2_JSON: &str = r#"{"schema_version":1,"camera_id":2,"image_size":{"width":640,"height":480},"k":[500.0,0.0,100.0,0.0,500.0,200.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}"#;

const CAMERA_7_JSON: &str = r#"{"schema_version":1,"camera_id":7,"image_size":{"width":640,"height":480},"k":[500.0,0.0,100.0,0.0,500.0,200.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}"#;

const VALID_RENDER_REQUEST_JSON: &str = concat!(
    r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":9,"accepted_attempt":2,"seed_fingerprint":"seed-abc","bins":[{"schema_version":1,"id":"bin","inner_dimensions_m":[1.0,2.0,3.0],"floor_thickness_m":0.1,"wall_thickness_m":0.2,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel","render_material":{"base_color_srgb":[0.6,0.6,0.62,1.0],"metallic":0.0,"roughness":0.6}}],"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":["#,
    r#"{"schema_version":1,"camera_id":2,"image_size":{"width":640,"height":480},"k":[500.0,0.0,100.0,0.0,500.0,200.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0},"#,
    r#"{"schema_version":1,"camera_id":7,"image_size":{"width":640,"height":480},"k":[500.0,0.0,100.0,0.0,500.0,200.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}],"appearance":"#,
    r#"{"schema_version":1,"background_srgb":[0.1,0.2,0.3,1.0],"ambient_light":{"schema_version":1,"color":[0.4,0.5,0.6],"intensity":0.25},"directional_lights":[{"schema_version":1,"direction_world":[1.0,0.0,0.0],"color":[1.0,2.0,3.0],"illuminance_lux":100.0},{"schema_version":1,"direction_world":[0.0,-1.0,0.0],"color":[4.0,5.0,6.0],"illuminance_lux":200.0}],"point_lights":[{"schema_version":1,"position_world_m":[3.0,2.0,1.0],"color":[7.0,8.0,9.0],"luminous_power_lm":300.0},{"schema_version":1,"position_world_m":[-3.0,-2.0,-1.0],"color":[10.0,11.0,12.0],"luminous_power_lm":400.0}]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true},"png_compression":"fast"}"#,
);

fn assert_same_float(actual: f32, expected: f32) {
    assert_eq!(actual.to_bits(), expected.to_bits());
}

fn assert_same_float_array<const N: usize>(actual: [f32; N], expected: [f32; N]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert_same_float(actual, expected);
    }
}

#[test]
fn srgb_color_checks_each_inclusive_component_boundary_and_wire_value() {
    let color = SrgbColor::try_new([-0.0, 0.25, 0.75, 1.0]).unwrap();

    assert_same_float_array(color.rgba(), [0.0, 0.25, 0.75, 1.0]);
    assert_eq!(
        serde_json::to_string(&color).unwrap(),
        "[0.0,0.25,0.75,1.0]"
    );
    assert_eq!(
        serde_json::from_str::<SrgbColor>("[0.0,0.25,0.75,1.0]").unwrap(),
        color
    );

    for (rgba, invalid_index) in [
        ([-0.000_001, 0.0, 0.0, 1.0], 0),
        ([0.0, 1.000_001, 0.0, 1.0], 1),
        ([0.0, 0.0, f32::NAN, 1.0], 2),
        ([0.0, 0.0, 0.0, f32::INFINITY], 3),
        ([0.0, 0.0, f32::NEG_INFINITY, 1.0], 2),
    ] {
        let error = SrgbColor::try_new(rgba).unwrap_err();
        assert_eq!(error.index(), invalid_index);
        assert_eq!(error.value().to_bits(), rgba[invalid_index].to_bits());
    }

    assert!(serde_json::from_str::<SrgbColor>("[-0.1,0.0,0.0,1.0]").is_err());
    assert!(serde_json::from_str::<SrgbColor>("[0.0,0.0,0.0,1.1]").is_err());
}

#[test]
fn linear_rgb_accepts_unbounded_nonnegative_components_and_rejects_invalid_values() {
    let color = LinearRgb::try_new([-0.0, 1.0, f32::MAX]).unwrap();

    assert_same_float_array(color.rgb(), [0.0, 1.0, f32::MAX]);
    assert_same_float_array(
        serde_json::from_str::<LinearRgb>("[0.0,1.0,1000000.0]")
            .unwrap()
            .rgb(),
        [0.0, 1.0, 1_000_000.0],
    );

    for (rgb, invalid_index) in [
        ([-0.000_001, 0.0, 0.0], 0),
        ([0.0, f32::NAN, 0.0], 1),
        ([0.0, 0.0, f32::INFINITY], 2),
        ([0.0, f32::NEG_INFINITY, 0.0], 1),
    ] {
        let error = LinearRgb::try_new(rgb).unwrap_err();
        assert_eq!(error.index(), invalid_index);
        assert_eq!(error.value().to_bits(), rgb[invalid_index].to_bits());
    }

    assert!(serde_json::from_str::<LinearRgb>("[0.0,-0.1,1.0]").is_err());
}

#[test]
fn nonnegative_finite_checks_zero_and_large_finite_boundaries() {
    let zero = NonNegativeFinite::try_new(-0.0).unwrap();
    let large = NonNegativeFinite::try_new(f32::MAX).unwrap();

    assert_same_float(zero.get(), 0.0);
    assert_same_float(large.get(), f32::MAX);
    assert_eq!(serde_json::to_string(&zero).unwrap(), "0.0");
    assert_same_float(
        serde_json::from_str::<NonNegativeFinite>("123.5")
            .unwrap()
            .get(),
        123.5,
    );

    for invalid in [-0.000_001, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let error = NonNegativeFinite::try_new(invalid).unwrap_err();
        assert_eq!(error.value().to_bits(), invalid.to_bits());
    }

    assert!(serde_json::from_str::<NonNegativeFinite>("-0.1").is_err());
}

#[test]
fn checked_appearance_errors_are_standard_errors() {
    fn assert_error<T: std::error::Error>() {}

    assert_error::<InvalidSrgbColorError>();
    assert_error::<InvalidLinearRgbError>();
    assert_error::<InvalidNonNegativeFiniteError>();
    assert_error::<InvalidDirectionalLightError>();
}

fn linear_rgb(rgb: [f32; 3]) -> LinearRgb {
    LinearRgb::try_new(rgb).unwrap()
}

fn nonnegative(value: f32) -> NonNegativeFinite {
    NonNegativeFinite::try_new(value).unwrap()
}

fn metres(values: [f32; 3]) -> [Meters; 3] {
    values.map(|value| Meters::new(value).unwrap())
}

fn valid_appearance() -> AppearanceSnapshot {
    AppearanceSnapshot::new(
        SrgbColor::try_new([0.1, 0.2, 0.3, 1.0]).unwrap(),
        AmbientLight::new(linear_rgb([0.4, 0.5, 0.6]), nonnegative(0.25)),
        vec![
            DirectionalLight::try_new(
                [2.0, 0.0, 0.0],
                linear_rgb([1.0, 2.0, 3.0]),
                nonnegative(100.0),
            )
            .unwrap(),
            DirectionalLight::try_new(
                [0.0, -3.0, -0.0],
                linear_rgb([4.0, 5.0, 6.0]),
                nonnegative(200.0),
            )
            .unwrap(),
        ],
        vec![
            PointLight::new(
                metres([3.0, 2.0, 1.0]),
                linear_rgb([7.0, 8.0, 9.0]),
                nonnegative(300.0),
            ),
            PointLight::new(
                metres([-3.0, -2.0, -1.0]),
                linear_rgb([10.0, 11.0, 12.0]),
                nonnegative(400.0),
            ),
        ],
    )
}

#[test]
fn directional_light_normalizes_finite_nonzero_input_and_canonicalizes_zero() {
    let light = DirectionalLight::try_new(
        [3.0, -4.0, -0.0],
        linear_rgb([0.5, 1.0, 1.5]),
        nonnegative(1_000.0),
    )
    .unwrap();

    assert_eq!(light.schema_version(), SchemaVersion::current());
    assert_same_float_array(light.direction_world(), [0.6, -0.8, 0.0]);
    assert_eq!(light.color(), linear_rgb([0.5, 1.0, 1.5]));
    assert_same_float(light.illuminance_lux().get(), 1_000.0);

    let huge = DirectionalLight::try_new(
        [f32::MAX, f32::MAX, 0.0],
        linear_rgb([1.0, 1.0, 1.0]),
        nonnegative(1.0),
    )
    .expect("finite nonzero directions remain valid even when a naive norm would overflow");
    let expected = 0.5_f32.sqrt();
    assert_same_float_array(huge.direction_world(), [expected, expected, 0.0]);
}

#[test]
fn directional_light_rejects_zero_and_each_nonfinite_direction() {
    assert_eq!(
        DirectionalLight::try_new(
            [0.0, -0.0, 0.0],
            linear_rgb([1.0, 1.0, 1.0]),
            nonnegative(1.0),
        ),
        Err(InvalidDirectionalLightError::ZeroDirection)
    );

    for (direction, invalid_index) in [
        ([f32::NAN, 0.0, 1.0], 0),
        ([1.0, f32::INFINITY, 0.0], 1),
        ([1.0, 0.0, f32::NEG_INFINITY], 2),
    ] {
        assert!(matches!(
            DirectionalLight::try_new(
                direction,
                linear_rgb([1.0, 1.0, 1.0]),
                nonnegative(1.0),
            ),
            Err(InvalidDirectionalLightError::NonFiniteDirection { index, value })
                if index == invalid_index && value.to_bits() == direction[invalid_index].to_bits()
        ));
    }

    let zero_json = r#"{"schema_version":1,"direction_world":[0.0,0.0,0.0],"color":[1.0,1.0,1.0],"illuminance_lux":1.0}"#;
    assert!(serde_json::from_str::<DirectionalLight>(zero_json).is_err());
}

#[test]
fn appearance_json_is_exact_schema_first_preserves_light_order_and_round_trips() {
    let appearance = valid_appearance();

    assert_eq!(
        serde_json::to_string(&appearance).unwrap(),
        VALID_APPEARANCE_JSON
    );
    assert_eq!(
        serde_json::from_str::<AppearanceSnapshot>(VALID_APPEARANCE_JSON).unwrap(),
        appearance
    );
    assert_eq!(appearance.schema_version(), SchemaVersion::current());
    assert_same_float_array(appearance.background_srgb().rgba(), [0.1, 0.2, 0.3, 1.0]);
    assert_same_float_array(appearance.ambient_light().color().rgb(), [0.4, 0.5, 0.6]);
    assert_same_float_array(
        appearance.directional_lights()[0].direction_world(),
        [1.0, 0.0, 0.0],
    );
    assert_same_float_array(
        appearance.directional_lights()[1].direction_world(),
        [0.0, -1.0, 0.0],
    );
    assert_same_float_array(
        appearance.point_lights()[0]
            .position_world_m()
            .map(Meters::get),
        [3.0, 2.0, 1.0],
    );
    assert_same_float_array(
        appearance.point_lights()[1]
            .position_world_m()
            .map(Meters::get),
        [-3.0, -2.0, -1.0],
    );
}

#[test]
fn appearance_wire_rejects_unknown_fields_and_nested_schema_version_two() {
    assert!(
        serde_json::from_str::<AppearanceSnapshot>(&VALID_APPEARANCE_JSON.replacen(
            "\"background_srgb\"",
            "\"extra\":true,\"background_srgb\"",
            1,
        ),)
        .is_err()
    );

    for marker in [
        "\"ambient_light\":{\"schema_version\":1",
        "\"directional_lights\":[{\"schema_version\":1",
        "\"point_lights\":[{\"schema_version\":1",
    ] {
        let invalid = VALID_APPEARANCE_JSON.replacen(
            marker,
            &marker.replace("schema_version\":1", "schema_version\":2"),
            1,
        );
        assert!(serde_json::from_str::<AppearanceSnapshot>(&invalid).is_err());
    }
}

#[test]
fn render_channels_reject_all_false_and_have_schema_first_checked_wire_values() {
    assert!(RenderChannels::try_new(false, false, false).is_err());
    assert!(
        serde_json::from_str::<RenderChannels>(
            r#"{"schema_version":1,"rgb":false,"depth":false,"instance":false}"#,
        )
        .is_err()
    );

    let channels = RenderChannels::try_new(true, false, true).unwrap();
    assert_eq!(channels.schema_version(), SchemaVersion::current());
    assert!(channels.rgb());
    assert!(!channels.depth());
    assert!(channels.instance());
    assert_eq!(
        serde_json::to_string(&channels).unwrap(),
        r#"{"schema_version":1,"rgb":true,"depth":false,"instance":true}"#
    );
    assert_eq!(
        serde_json::from_str::<RenderChannels>(
            r#"{"schema_version":1,"rgb":true,"depth":false,"instance":true}"#,
        )
        .unwrap(),
        channels
    );
}

#[test]
fn render_channel_containment_matches_the_complete_nonempty_truth_table() {
    let sets = [
        RenderChannels::try_new(true, false, false).unwrap(),
        RenderChannels::try_new(false, true, false).unwrap(),
        RenderChannels::try_new(false, false, true).unwrap(),
        RenderChannels::try_new(true, true, false).unwrap(),
        RenderChannels::try_new(true, false, true).unwrap(),
        RenderChannels::try_new(false, true, true).unwrap(),
        RenderChannels::try_new(true, true, true).unwrap(),
    ];
    let expected = [
        [true, false, false, false, false, false, false],
        [false, true, false, false, false, false, false],
        [false, false, true, false, false, false, false],
        [true, true, false, true, false, false, false],
        [true, false, true, false, true, false, false],
        [false, true, true, false, false, true, false],
        [true, true, true, true, true, true, true],
    ];

    for (available_index, available) in sets.into_iter().enumerate() {
        for (required_index, required) in sets.into_iter().enumerate() {
            assert_eq!(
                available.contains(required),
                expected[available_index][required_index],
                "available set {available_index}, required set {required_index}",
            );
        }
    }
}

#[test]
fn render_channels_reject_unknown_fields_and_schema_version_two() {
    assert!(
        serde_json::from_str::<RenderChannels>(
            r#"{"schema_version":1,"rgb":true,"depth":false,"instance":false,"extra":true}"#,
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<RenderChannels>(
            r#"{"schema_version":2,"rgb":true,"depth":false,"instance":false}"#,
        )
        .is_err()
    );
}

fn valid_scene() -> SceneSnapshot {
    serde_json::from_str(VALID_SCENE_JSON).unwrap()
}

fn camera(json: &str) -> CameraSnapshot {
    serde_json::from_str(json).unwrap()
}

fn all_channels() -> RenderChannels {
    RenderChannels::try_new(true, true, true).unwrap()
}

#[test]
fn render_request_constructor_sorts_cameras_and_serializes_exact_schema_order() {
    let request = RenderSceneRequest::try_new(
        valid_scene(),
        vec![camera(CAMERA_7_JSON), camera(CAMERA_2_JSON)],
        valid_appearance(),
        all_channels(),
    )
    .unwrap();

    assert_eq!(request.schema_version(), SchemaVersion::current());
    assert_eq!(request.scene(), &valid_scene());
    assert_eq!(
        request
            .cameras()
            .iter()
            .map(|camera| camera.camera_id())
            .collect::<Vec<_>>(),
        vec![CameraId::new(2), CameraId::new(7)]
    );
    assert_eq!(request.appearance(), &valid_appearance());
    assert_eq!(request.required_channels(), all_channels());
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        VALID_RENDER_REQUEST_JSON
    );
    assert_eq!(
        serde_json::from_str::<RenderSceneRequest>(VALID_RENDER_REQUEST_JSON).unwrap(),
        request
    );
}

#[test]
fn render_request_rejects_empty_and_duplicate_camera_ids() {
    assert_eq!(
        RenderSceneRequest::try_new(valid_scene(), vec![], valid_appearance(), all_channels(),),
        Err(InvalidRenderSceneRequestError::EmptyCameras)
    );
    assert!(matches!(
        RenderSceneRequest::try_new(
            valid_scene(),
            vec![camera(CAMERA_2_JSON), camera(CAMERA_2_JSON)],
            valid_appearance(),
            all_channels(),
        ),
        Err(InvalidRenderSceneRequestError::DuplicateCameraId { camera_id })
            if camera_id == CameraId::new(2)
    ));

    let empty_wire = format!(
        r#"{{"schema_version":1,"scene":{VALID_SCENE_JSON},"cameras":[],"appearance":{VALID_APPEARANCE_JSON},"required_channels":{{"schema_version":1,"rgb":true,"depth":true,"instance":true}}}}"#,
    );
    let duplicate_wire = format!(
        r#"{{"schema_version":1,"scene":{VALID_SCENE_JSON},"cameras":[{CAMERA_2_JSON},{CAMERA_2_JSON}],"appearance":{VALID_APPEARANCE_JSON},"required_channels":{{"schema_version":1,"rgb":true,"depth":true,"instance":true}}}}"#,
    );
    assert!(serde_json::from_str::<RenderSceneRequest>(&empty_wire).is_err());
    assert!(serde_json::from_str::<RenderSceneRequest>(&duplicate_wire).is_err());
}

#[test]
fn render_request_wire_rejects_unsorted_cameras_without_canonicalizing() {
    let unsorted = format!(
        r#"{{"schema_version":1,"scene":{VALID_SCENE_JSON},"cameras":[{CAMERA_7_JSON},{CAMERA_2_JSON}],"appearance":{VALID_APPEARANCE_JSON},"required_channels":{{"schema_version":1,"rgb":true,"depth":true,"instance":true}}}}"#,
    );

    assert!(serde_json::from_str::<RenderSceneRequest>(&unsorted).is_err());
}

#[test]
fn render_request_wire_rejects_unknown_fields_and_every_nested_version_two() {
    assert!(
        serde_json::from_str::<RenderSceneRequest>(&VALID_RENDER_REQUEST_JSON.replacen(
            "\"scene\"",
            "\"extra\":true,\"scene\"",
            1,
        ),)
        .is_err()
    );

    let version_markers = [
        "{\"schema_version\":1,\"scene\"",
        "\"scene\":{\"schema_version\":1",
        "\"cameras\":[{\"schema_version\":1",
        "\"appearance\":{\"schema_version\":1",
        "\"required_channels\":{\"schema_version\":1",
    ];
    for marker in version_markers {
        let invalid = VALID_RENDER_REQUEST_JSON.replacen(
            marker,
            &marker.replace("schema_version\":1", "schema_version\":2"),
            1,
        );
        assert!(
            serde_json::from_str::<RenderSceneRequest>(&invalid).is_err(),
            "accepted schema version 2 at marker {marker}",
        );
    }
}

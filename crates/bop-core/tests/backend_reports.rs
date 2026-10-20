//! Behavioral tests for renderer capability and report contracts.

use std::{collections::BTreeMap, num::NonZeroU32};

use bop_core::{
    BackendInfo, BackendKind, InvalidBackendInfoError, NonNegativeFinite, RenderChannels,
    RenderReport, RenderStageTimings, RendererCapabilities, SceneId, SchemaVersion,
};

const BACKEND_JSON: &str = r#"{"schema_version":1,"kind":"wgpu","implementation_version":"0.1.0","device_name":"RTX 6000","properties":{"adapter":"Ada","driver":"550"}}"#;
const CHANNELS_JSON: &str = r#"{"schema_version":1,"rgb":true,"depth":true,"instance":false}"#;
const TIMINGS_JSON: &str = r#"{"schema_version":1,"prepare_seconds":0.25,"render_seconds":1.5,"readback_seconds":0.125,"encode_seconds":0.5}"#;
const REPORT_JSON: &str = r#"{"schema_version":1,"scene_id":42,"backend":{"schema_version":1,"kind":"wgpu","implementation_version":"0.1.0","device_name":"RTX 6000","properties":{"adapter":"Ada","driver":"550"}},"camera_count":2,"channels":{"schema_version":1,"rgb":true,"depth":true,"instance":false},"timings":{"schema_version":1,"prepare_seconds":0.25,"render_seconds":1.5,"readback_seconds":0.125,"encode_seconds":0.5},"worker_restart_count":3}"#;

fn nonnegative(value: f32) -> NonNegativeFinite {
    NonNegativeFinite::try_new(value).unwrap()
}

fn channels(rgb: bool, depth: bool, instance: bool) -> RenderChannels {
    RenderChannels::try_new(rgb, depth, instance).unwrap()
}

fn backend() -> BackendInfo {
    let mut properties = BTreeMap::new();
    properties.insert("driver".to_owned(), "550".to_owned());
    properties.insert("adapter".to_owned(), "Ada".to_owned());
    BackendInfo::try_new(
        BackendKind::Wgpu,
        "0.1.0",
        Some("RTX 6000".to_owned()),
        properties,
    )
    .unwrap()
}

fn timings() -> RenderStageTimings {
    RenderStageTimings::new(
        nonnegative(0.25),
        nonnegative(1.5),
        nonnegative(0.125),
        nonnegative(0.5),
    )
}

#[test]
fn gpu_time_is_optional_and_distinct_from_wall_clock_stages() {
    let without_gpu = timings();
    assert_eq!(without_gpu.gpu_seconds(), None);
    assert!(
        !serde_json::to_string(&without_gpu)
            .unwrap()
            .contains("gpu_seconds")
    );

    let with_gpu = without_gpu.with_gpu_seconds(Some(nonnegative(0.75)));
    assert_eq!(with_gpu.gpu_seconds(), Some(nonnegative(0.75)));
    let json = serde_json::to_string(&with_gpu).unwrap();
    assert!(json.contains(r#""gpu_seconds":0.75"#));
    assert_eq!(
        serde_json::from_str::<RenderStageTimings>(&json).unwrap(),
        with_gpu
    );
}

#[test]
fn backend_kind_uses_exact_snake_case_wire_values() {
    let cases = [
        (BackendKind::Wgpu, r#""wgpu""#),
        (BackendKind::BlenderProc, r#""blenderproc""#),
    ];
    for (kind, expected) in cases {
        assert_eq!(serde_json::to_string(&kind).unwrap(), expected);
        assert_eq!(serde_json::from_str::<BackendKind>(expected).unwrap(), kind);
    }

    for invalid in [
        r#"""#,
        r#""Wgpu""#,
        r#""WGPU""#,
        r#""blender_proc""#,
        r#""blender-proc""#,
        r#""BlenderProc""#,
        r#""cpu""#,
    ] {
        assert!(
            serde_json::from_str::<BackendKind>(invalid).is_err(),
            "accepted invalid backend kind {invalid}",
        );
    }
}

#[test]
fn backend_info_is_schema_first_and_orders_properties_deterministically() {
    let backend = backend();

    assert_eq!(backend.schema_version(), SchemaVersion::current());
    assert_eq!(backend.kind(), BackendKind::Wgpu);
    assert_eq!(backend.implementation_version(), "0.1.0");
    assert_eq!(backend.device_name(), Some("RTX 6000"));
    assert_eq!(
        backend
            .properties()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["adapter", "driver"],
    );
    assert_eq!(serde_json::to_string(&backend).unwrap(), BACKEND_JSON);
    assert_eq!(
        serde_json::from_str::<BackendInfo>(BACKEND_JSON).unwrap(),
        backend
    );

    let without_device =
        BackendInfo::try_new(BackendKind::BlenderProc, "3.4.4", None, BTreeMap::new()).unwrap();
    assert_eq!(without_device.device_name(), None);
    assert_eq!(
        serde_json::to_string(&without_device).unwrap(),
        r#"{"schema_version":1,"kind":"blenderproc","implementation_version":"3.4.4","device_name":null,"properties":{}}"#,
    );
}

#[test]
fn backend_info_rejects_blank_versions_devices_and_property_fields() {
    for invalid in ["", " ", "\t\n"] {
        assert!(matches!(
            BackendInfo::try_new(
                BackendKind::Wgpu,
                invalid,
                None,
                BTreeMap::new(),
            ),
            Err(InvalidBackendInfoError::InvalidImplementationVersion { value })
                if value == invalid
        ));
    }

    for invalid in ["", " ", "\t\n"] {
        assert!(matches!(
            BackendInfo::try_new(
                BackendKind::Wgpu,
                "1.0",
                Some(invalid.to_owned()),
                BTreeMap::new(),
            ),
            Err(InvalidBackendInfoError::InvalidDeviceName { value }) if value == invalid
        ));
    }

    for invalid in ["", " \t"] {
        let mut invalid_key = BTreeMap::new();
        invalid_key.insert(invalid.to_owned(), "value".to_owned());
        assert!(matches!(
            BackendInfo::try_new(BackendKind::Wgpu, "1.0", None, invalid_key),
            Err(InvalidBackendInfoError::InvalidPropertyKey { key }) if key == invalid
        ));
    }

    for invalid in ["", " \n"] {
        let mut invalid_value = BTreeMap::new();
        invalid_value.insert("driver".to_owned(), invalid.to_owned());
        assert!(matches!(
            BackendInfo::try_new(BackendKind::Wgpu, "1.0", None, invalid_value),
            Err(InvalidBackendInfoError::InvalidPropertyValue { key, value })
                if key == "driver" && value == invalid
        ));
    }

    for invalid in [
        r#"{"schema_version":1,"kind":"wgpu","implementation_version":" ","device_name":null,"properties":{}}"#,
        r#"{"schema_version":1,"kind":"wgpu","implementation_version":"1.0","device_name":" ","properties":{}}"#,
        r#"{"schema_version":1,"kind":"wgpu","implementation_version":"1.0","device_name":null,"properties":{"":"value"}}"#,
        r#"{"schema_version":1,"kind":"wgpu","implementation_version":"1.0","device_name":null,"properties":{" ":"value"}}"#,
        r#"{"schema_version":1,"kind":"wgpu","implementation_version":"1.0","device_name":null,"properties":{"driver":""}}"#,
        r#"{"schema_version":1,"kind":"wgpu","implementation_version":"1.0","device_name":null,"properties":{"driver":" "}}"#,
    ] {
        assert!(
            serde_json::from_str::<BackendInfo>(invalid).is_err(),
            "accepted invalid backend info {invalid}",
        );
    }
}

#[test]
fn capabilities_check_channel_containment_and_camera_limits() {
    let available = channels(true, true, false);
    let limited = RendererCapabilities::new(available, NonZeroU32::new(2));

    assert_eq!(limited.schema_version(), SchemaVersion::current());
    assert_eq!(limited.channels(), available);
    assert_eq!(limited.max_cameras_per_scene(), NonZeroU32::new(2));
    assert!(!limited.supports(channels(true, false, false), 0));
    assert!(limited.supports(channels(true, false, false), 1));
    assert!(limited.supports(channels(true, true, false), 2));
    assert!(!limited.supports(channels(true, false, false), 3));
    assert!(!limited.supports(channels(true, false, true), 1));

    let unlimited = RendererCapabilities::new(available, None);
    assert!(!unlimited.supports(channels(true, false, false), 0));
    assert!(unlimited.supports(channels(true, false, false), u32::MAX));
    assert_eq!(
        serde_json::to_string(&unlimited).unwrap(),
        r#"{"schema_version":1,"channels":{"schema_version":1,"rgb":true,"depth":true,"instance":false},"max_cameras_per_scene":null}"#,
    );
    assert_eq!(
        serde_json::from_str::<RendererCapabilities>(
            r#"{"schema_version":1,"channels":{"schema_version":1,"rgb":true,"depth":true,"instance":false},"max_cameras_per_scene":null}"#,
        )
        .unwrap(),
        unlimited,
    );
}

#[test]
fn timing_totals_are_checked_for_f32_overflow() {
    let timings = timings();
    assert_eq!(timings.schema_version(), SchemaVersion::current());
    assert_eq!(timings.prepare_seconds(), nonnegative(0.25));
    assert_eq!(timings.render_seconds(), nonnegative(1.5));
    assert_eq!(timings.readback_seconds(), nonnegative(0.125));
    assert_eq!(timings.encode_seconds(), nonnegative(0.5));
    assert_eq!(timings.total_seconds(), Some(nonnegative(2.375)));
    assert_eq!(serde_json::to_string(&timings).unwrap(), TIMINGS_JSON);
    assert_eq!(
        serde_json::from_str::<RenderStageTimings>(TIMINGS_JSON).unwrap(),
        timings,
    );

    let overflowing = RenderStageTimings::new(
        nonnegative(f32::MAX),
        nonnegative(f32::MAX),
        nonnegative(0.0),
        nonnegative(0.0),
    );
    assert_eq!(overflowing.total_seconds(), None);
}

#[test]
fn render_report_is_schema_first_and_round_trips_exactly() {
    let report = RenderReport::new(
        SceneId::new(42),
        backend(),
        NonZeroU32::new(2).unwrap(),
        channels(true, true, false),
        timings(),
        3,
    );

    assert_eq!(report.schema_version(), SchemaVersion::current());
    assert_eq!(report.scene_id(), SceneId::new(42));
    assert_eq!(report.backend(), &backend());
    assert_eq!(report.camera_count().get(), 2);
    assert_eq!(report.channels(), channels(true, true, false));
    assert_eq!(report.timings(), timings());
    assert_eq!(report.worker_restart_count(), 3);
    assert_eq!(serde_json::to_string(&report).unwrap(), REPORT_JSON);
    assert_eq!(
        serde_json::from_str::<RenderReport>(REPORT_JSON).unwrap(),
        report
    );

    let zero_camera_count = REPORT_JSON.replace("\"camera_count\":2", "\"camera_count\":0");
    assert!(serde_json::from_str::<RenderReport>(&zero_camera_count).is_err());
}

#[test]
fn versioned_objects_reject_unknown_fields() {
    let invalid_values = [
        BACKEND_JSON.replacen("\"kind\"", "\"extra\":true,\"kind\"", 1),
        format!(
            r#"{{"schema_version":1,"channels":{CHANNELS_JSON},"max_cameras_per_scene":2,"extra":true}}"#,
        ),
        TIMINGS_JSON.replacen(
            "\"prepare_seconds\"",
            "\"extra\":true,\"prepare_seconds\"",
            1,
        ),
        REPORT_JSON.replacen("\"scene_id\"", "\"extra\":true,\"scene_id\"", 1),
    ];

    assert!(serde_json::from_str::<BackendInfo>(&invalid_values[0]).is_err());
    assert!(serde_json::from_str::<RendererCapabilities>(&invalid_values[1]).is_err());
    assert!(serde_json::from_str::<RenderStageTimings>(&invalid_values[2]).is_err());
    assert!(serde_json::from_str::<RenderReport>(&invalid_values[3]).is_err());
}

#[test]
fn versioned_objects_reject_schema_version_two_at_every_nested_boundary() {
    assert!(
        serde_json::from_str::<BackendInfo>(&BACKEND_JSON.replacen(
            "\"schema_version\":1",
            "\"schema_version\":2",
            1,
        ))
        .is_err()
    );

    let capabilities =
        format!(r#"{{"schema_version":1,"channels":{CHANNELS_JSON},"max_cameras_per_scene":2}}"#);
    for marker in [
        "{\"schema_version\":1,\"channels\"",
        "\"channels\":{\"schema_version\":1",
    ] {
        let invalid = capabilities.replacen(
            marker,
            &marker.replace("schema_version\":1", "schema_version\":2"),
            1,
        );
        assert!(serde_json::from_str::<RendererCapabilities>(&invalid).is_err());
    }

    assert!(
        serde_json::from_str::<RenderStageTimings>(&TIMINGS_JSON.replacen(
            "\"schema_version\":1",
            "\"schema_version\":2",
            1,
        ))
        .is_err()
    );

    for marker in [
        "{\"schema_version\":1,\"scene_id\"",
        "\"backend\":{\"schema_version\":1",
        "\"channels\":{\"schema_version\":1",
        "\"timings\":{\"schema_version\":1",
    ] {
        let invalid = REPORT_JSON.replacen(
            marker,
            &marker.replace("schema_version\":1", "schema_version\":2"),
            1,
        );
        assert!(
            serde_json::from_str::<RenderReport>(&invalid).is_err(),
            "accepted schema version 2 at marker {marker}",
        );
    }
}

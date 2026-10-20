//! Public `BlenderProc` worker protocol and supervision contract.

use bop_core::BlenderProcSampling;
use bop_render_blenderproc::{PROTOCOL_VERSION, WorkerMessage, WorkerReply};

#[test]
fn protocol_messages_are_versioned_and_strict() {
    let hello: WorkerMessage = serde_json::from_str(
        r#"{"protocol_version":1,"type":"hello","device_selection":"AUTO","samples":128,"noise_threshold":0.01,"denoiser":"oidn","glossy_bounces":1}"#,
    )
    .unwrap();
    assert_eq!(
        hello,
        WorkerMessage::hello("AUTO", BlenderProcSampling::DEFAULT)
    );
    assert_eq!(PROTOCOL_VERSION, 1);

    let pinned = WorkerMessage::hello_for_worker("AUTO", BlenderProcSampling::DEFAULT, 1, 2);
    let wire = serde_json::to_string(&pinned).unwrap();
    assert!(wire.contains(r#""worker_index":1,"worker_count":2"#));
    assert_eq!(serde_json::from_str::<WorkerMessage>(&wire).unwrap(), pinned);

    let ready: WorkerReply = serde_json::from_str(
        r#"{"protocol_version":1,"type":"ready","implementation_version":"2.0","device_name":"CPU"}"#,
    )
    .unwrap();
    assert_eq!(ready.implementation_version(), Some("2.0"));
    assert!(serde_json::from_str::<WorkerReply>(
        r#"{"protocol_version":1,"type":"ready","implementation_version":"2.0","device_name":"CPU","surprise":1}"#
    )
    .is_err());
}

#[test]
fn incompatible_protocol_versions_are_rejected() {
    assert!(serde_json::from_str::<WorkerReply>(
        r#"{"protocol_version":2,"type":"ready","implementation_version":"2.0","device_name":null}"#
    )
    .is_err());
}

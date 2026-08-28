//! Public `BlenderProc` worker protocol and supervision contract.

use bpp_render_blenderproc::{PROTOCOL_VERSION, WorkerMessage, WorkerReply};

#[test]
fn protocol_messages_are_versioned_and_strict() {
    let hello: WorkerMessage =
        serde_json::from_str(r#"{"protocol_version":1,"type":"hello","device_selection":"AUTO"}"#)
            .unwrap();
    assert_eq!(hello, WorkerMessage::hello("AUTO"));
    assert_eq!(PROTOCOL_VERSION, 1);

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

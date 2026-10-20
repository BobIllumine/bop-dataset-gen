//! Behavioral tests for public identifier contracts.

use bop_core::{AssetId, AttemptId, CameraId, InstanceId, SceneId};

#[test]
fn numeric_identifiers_preserve_their_domain_values() {
    assert_eq!(SceneId::new(9).get(), 9);
    assert_eq!(AttemptId::new(3).get(), 3);
    assert_eq!(CameraId::new(7).get(), 7);
}

#[test]
fn asset_id_rejects_empty_or_whitespace_only_values() {
    assert!(AssetId::new("").is_none());
    assert!(AssetId::new(" \t\n").is_none());
}

#[test]
fn asset_id_preserves_a_nonempty_value() {
    let id = AssetId::new("widget-a").expect("nonempty asset IDs are valid");

    assert_eq!(id.as_str(), "widget-a");
}

#[test]
fn instance_id_rejects_zero_reserved_for_background() {
    assert!(InstanceId::new(0).is_none());
}

#[test]
fn instance_id_preserves_nonzero_render_id() {
    let id = InstanceId::new(42).expect("42 is a valid foreground ID");

    assert_eq!(id.get(), 42);
}

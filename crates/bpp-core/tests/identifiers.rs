//! Behavioral tests for public identifier contracts.

use bpp_core::InstanceId;

#[test]
fn instance_id_rejects_zero_reserved_for_background() {
    assert!(InstanceId::new(0).is_none());
}

#[test]
fn instance_id_preserves_nonzero_render_id() {
    let id = InstanceId::new(42).expect("42 is a valid foreground ID");

    assert_eq!(id.get(), 42);
}

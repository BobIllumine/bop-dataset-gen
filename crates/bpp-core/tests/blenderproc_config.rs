//! Programmatic `BlenderProc` worker configuration contract.

use std::path::Path;

use bpp_core::{BlenderProcRendererConfig, InvalidBlenderProcRendererConfigError};

#[test]
fn programmatic_blenderproc_config_is_checked_and_complete() {
    let config = BlenderProcRendererConfig::try_new(
        "blenderproc",
        "workers/blenderproc_worker.py",
        "AUTO",
        10.0,
        300.0,
        16_384,
    )
    .unwrap();
    assert_eq!(config.executable_path(), Path::new("blenderproc"));
    assert_eq!(
        config.worker_path(),
        Path::new("workers/blenderproc_worker.py")
    );
    assert_eq!(config.device_selection(), "AUTO");
    assert_eq!(config.handshake_timeout_s().to_bits(), 10.0_f32.to_bits());
    assert_eq!(config.request_timeout_s().to_bits(), 300.0_f32.to_bits());
    assert_eq!(config.stderr_tail_capacity(), 16_384);
}

#[test]
fn programmatic_blenderproc_config_rejects_every_invalid_boundary() {
    let make = |executable, worker, device, handshake, request, tail| {
        BlenderProcRendererConfig::try_new(executable, worker, device, handshake, request, tail)
    };
    assert_eq!(
        make("", "w", "A", 1.0, 1.0, 1),
        Err(InvalidBlenderProcRendererConfigError::EmptyExecutablePath)
    );
    assert_eq!(
        make("b", "", "A", 1.0, 1.0, 1),
        Err(InvalidBlenderProcRendererConfigError::EmptyWorkerPath)
    );
    assert_eq!(
        make("b", "w", " ", 1.0, 1.0, 1),
        Err(InvalidBlenderProcRendererConfigError::BlankDeviceSelection)
    );
    assert_eq!(
        make("b", "w", "A", 0.0, 1.0, 1),
        Err(InvalidBlenderProcRendererConfigError::InvalidHandshakeTimeout)
    );
    assert_eq!(
        make("b", "w", "A", 1.0, f32::NAN, 1),
        Err(InvalidBlenderProcRendererConfigError::InvalidRequestTimeout)
    );
    assert_eq!(
        make("b", "w", "A", 1.0, 1.0, 0),
        Err(InvalidBlenderProcRendererConfigError::ZeroStderrTailCapacity)
    );
}

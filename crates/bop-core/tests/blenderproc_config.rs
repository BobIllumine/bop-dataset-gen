//! Programmatic `BlenderProc` worker configuration contract.

use std::path::Path;

use bop_core::{
    BlenderProcDenoiser, BlenderProcRendererConfig, BlenderProcSampling,
    InvalidBlenderProcRendererConfigError,
};

#[test]
fn programmatic_blenderproc_config_is_checked_and_complete() {
    let config = BlenderProcRendererConfig::try_new(
        "blenderproc",
        "tools/blenderproc-worker/blenderproc_worker.py",
        "AUTO",
        10.0,
        300.0,
        16_384,
    )
    .unwrap();
    assert_eq!(config.executable_path(), Path::new("blenderproc"));
    assert_eq!(
        config.worker_path(),
        Path::new("tools/blenderproc-worker/blenderproc_worker.py")
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

#[test]
fn sampling_defaults_to_128_samples_with_oidn_and_is_checked() {
    let config = BlenderProcRendererConfig::try_new("b", "w", "A", 1.0, 1.0, 1).unwrap();
    assert_eq!(config.sampling(), BlenderProcSampling::DEFAULT);
    assert_eq!(BlenderProcSampling::DEFAULT.samples(), 128);
    assert_eq!(
        BlenderProcSampling::DEFAULT.noise_threshold().to_bits(),
        0.01_f32.to_bits()
    );
    assert_eq!(
        BlenderProcSampling::DEFAULT.denoiser(),
        BlenderProcDenoiser::Oidn
    );

    let sampling = BlenderProcSampling::try_new(4096, 0.0, BlenderProcDenoiser::None).unwrap();
    assert_eq!(config.with_sampling(sampling).sampling(), sampling);
    assert_eq!(
        BlenderProcSampling::try_new(0, 0.01, BlenderProcDenoiser::Oidn),
        Err(InvalidBlenderProcRendererConfigError::ZeroSamples)
    );
    for threshold in [-0.1, f32::NAN, f32::INFINITY] {
        assert_eq!(
            BlenderProcSampling::try_new(64, threshold, BlenderProcDenoiser::Oidn),
            Err(InvalidBlenderProcRendererConfigError::InvalidNoiseThreshold)
        );
    }
}

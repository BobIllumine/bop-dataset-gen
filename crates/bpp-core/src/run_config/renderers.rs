//! Backend-specific renderer settings.

#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the parent's imports and helpers"
)]
use super::*;

/// A typed wgpu device preference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WgpuDevicePreference {
    /// Let wgpu choose the device class.
    Automatic,
    /// Prefer a CPU-backed device.
    Cpu,
    /// Prefer a GPU-backed device.
    Gpu,
}

/// A typed wgpu adapter power preference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WgpuAdapterPreference {
    /// Let wgpu choose the adapter preference.
    Automatic,
    /// Prefer a low-power adapter.
    LowPower,
    /// Prefer a high-performance adapter.
    HighPerformance,
}

/// Checked wgpu renderer options without renderer state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WgpuRendererConfig {
    pub(super) device_preference: WgpuDevicePreference,
    pub(super) adapter_preference: WgpuAdapterPreference,
    pub(super) shadow_map_size: u32,
    pub(super) rgb_fxaa: bool,
}

impl WgpuRendererConfig {
    /// Creates checked `wgpu` renderer options without initializing a device.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidWgpuRendererConfigError::ZeroShadowMapSize`] when the
    /// shadow-map resolution is zero.
    pub const fn try_new(
        device_preference: WgpuDevicePreference,
        adapter_preference: WgpuAdapterPreference,
        shadow_map_size: u32,
        rgb_fxaa: bool,
    ) -> Result<Self, InvalidWgpuRendererConfigError> {
        if shadow_map_size == 0 {
            return Err(InvalidWgpuRendererConfigError::ZeroShadowMapSize);
        }
        Ok(Self {
            device_preference,
            adapter_preference,
            shadow_map_size,
            rgb_fxaa,
        })
    }

    /// Returns the device preference.
    #[must_use]
    pub const fn device_preference(self) -> WgpuDevicePreference {
        self.device_preference
    }
    /// Returns the adapter preference.
    #[must_use]
    pub const fn adapter_preference(self) -> WgpuAdapterPreference {
        self.adapter_preference
    }
    /// Returns the nonzero shadow-map size.
    #[must_use]
    pub const fn shadow_map_size(self) -> u32 {
        self.shadow_map_size
    }
    /// Reports whether RGB FXAA is enabled.
    #[must_use]
    pub const fn rgb_fxaa(self) -> bool {
        self.rgb_fxaa
    }

    pub(super) fn try_from_wire(wire: WgpuRendererWire) -> Result<Self, RunConfigValidationError> {
        nonzero(wire.shadow_map_size, ConfigCountField::ShadowMapSize)?;
        Ok(Self {
            device_preference: match wire.device_preference {
                WgpuDevicePreferenceWire::Automatic => WgpuDevicePreference::Automatic,
                WgpuDevicePreferenceWire::Cpu => WgpuDevicePreference::Cpu,
                WgpuDevicePreferenceWire::Gpu => WgpuDevicePreference::Gpu,
            },
            adapter_preference: match wire.adapter_preference {
                WgpuAdapterPreferenceWire::Automatic => WgpuAdapterPreference::Automatic,
                WgpuAdapterPreferenceWire::LowPower => WgpuAdapterPreference::LowPower,
                WgpuAdapterPreferenceWire::HighPerformance => {
                    WgpuAdapterPreference::HighPerformance
                }
            },
            shadow_map_size: wire.shadow_map_size,
            rgb_fxaa: wire.rgb_fxaa,
        })
    }
}

/// Error returned when programmatic `wgpu` renderer options are invalid.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidWgpuRendererConfigError {
    /// Shadow maps require a nonzero square resolution.
    #[error("shadow_map_size must be nonzero")]
    ZeroShadowMapSize,
}

/// Checked `BlenderProc` worker process options without starting a process.
#[derive(Clone, Debug, PartialEq)]
pub struct BlenderProcRendererConfig {
    pub(super) executable_path: PathBuf,
    pub(super) worker_path: PathBuf,
    pub(super) device_selection: String,
    pub(super) handshake_timeout_s: f32,
    pub(super) request_timeout_s: f32,
    pub(super) stderr_tail_capacity: u32,
}

impl BlenderProcRendererConfig {
    /// Constructs checked programmatic worker options.
    ///
    /// # Errors
    ///
    /// Returns a field-specific error for invalid paths, text, deadlines, or bounds.
    pub fn try_new(
        executable_path: impl Into<PathBuf>,
        worker_path: impl Into<PathBuf>,
        device_selection: impl Into<String>,
        handshake_timeout_s: f32,
        request_timeout_s: f32,
        stderr_tail_capacity: u32,
    ) -> Result<Self, InvalidBlenderProcRendererConfigError> {
        let executable_path = executable_path.into();
        let worker_path = worker_path.into();
        let device_selection = device_selection.into();
        if executable_path.as_os_str().is_empty() {
            return Err(InvalidBlenderProcRendererConfigError::EmptyExecutablePath);
        }
        if worker_path.as_os_str().is_empty() {
            return Err(InvalidBlenderProcRendererConfigError::EmptyWorkerPath);
        }
        if device_selection.trim().is_empty() {
            return Err(InvalidBlenderProcRendererConfigError::BlankDeviceSelection);
        }
        if !handshake_timeout_s.is_finite() || handshake_timeout_s <= 0.0 {
            return Err(InvalidBlenderProcRendererConfigError::InvalidHandshakeTimeout);
        }
        if !request_timeout_s.is_finite() || request_timeout_s <= 0.0 {
            return Err(InvalidBlenderProcRendererConfigError::InvalidRequestTimeout);
        }
        if stderr_tail_capacity == 0 {
            return Err(InvalidBlenderProcRendererConfigError::ZeroStderrTailCapacity);
        }
        Ok(Self {
            executable_path,
            worker_path,
            device_selection,
            handshake_timeout_s,
            request_timeout_s,
            stderr_tail_capacity,
        })
    }

    /// Returns the nonempty executable path.
    #[must_use]
    pub fn executable_path(&self) -> &Path {
        &self.executable_path
    }
    /// Returns the nonempty worker path.
    #[must_use]
    pub fn worker_path(&self) -> &Path {
        &self.worker_path
    }
    /// Returns the nonblank device selection string.
    #[must_use]
    pub fn device_selection(&self) -> &str {
        &self.device_selection
    }
    /// Returns the positive finite handshake timeout in seconds.
    #[must_use]
    pub const fn handshake_timeout_s(&self) -> f32 {
        self.handshake_timeout_s
    }
    /// Returns the positive finite request timeout in seconds.
    #[must_use]
    pub const fn request_timeout_s(&self) -> f32 {
        self.request_timeout_s
    }
    /// Returns the nonzero stderr tail capacity in bytes.
    #[must_use]
    pub const fn stderr_tail_capacity(&self) -> u32 {
        self.stderr_tail_capacity
    }

    pub(super) fn try_from_wire(
        wire: BlenderProcRendererWire,
    ) -> Result<Self, RunConfigValidationError> {
        let executable_path = checked_path(
            wire.executable_path,
            ConfigPathField::BlenderProcExecutable,
            None,
        )?;
        let worker_path = checked_path(wire.worker_path, ConfigPathField::BlenderProcWorker, None)?;
        if wire.device_selection.trim().is_empty() {
            return Err(RunConfigValidationError::InvalidText {
                field: ConfigTextField::BlenderProcDeviceSelection,
            });
        }
        positive_f32(
            wire.handshake_timeout_s,
            ConfigScalarField::BlenderProcHandshakeTimeout,
        )?;
        positive_f32(
            wire.request_timeout_s,
            ConfigScalarField::BlenderProcRequestTimeout,
        )?;
        nonzero(
            wire.stderr_tail_capacity,
            ConfigCountField::StderrTailCapacity,
        )?;
        Ok(Self {
            executable_path,
            worker_path,
            device_selection: wire.device_selection,
            handshake_timeout_s: wire.handshake_timeout_s,
            request_timeout_s: wire.request_timeout_s,
            stderr_tail_capacity: wire.stderr_tail_capacity,
        })
    }
}

/// Error returned when programmatic `BlenderProc` options are invalid.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidBlenderProcRendererConfigError {
    /// Executable path is empty.
    #[error("executable_path must not be empty")]
    EmptyExecutablePath,
    /// Worker path is empty.
    #[error("worker_path must not be empty")]
    EmptyWorkerPath,
    /// Device selection is blank.
    #[error("device_selection must not be blank")]
    BlankDeviceSelection,
    /// Handshake deadline is invalid.
    #[error("handshake_timeout_s must be positive and finite")]
    InvalidHandshakeTimeout,
    /// Request deadline is invalid.
    #[error("request_timeout_s must be positive and finite")]
    InvalidRequestTimeout,
    /// Stderr diagnostic bound is zero.
    #[error("stderr_tail_capacity must be nonzero")]
    ZeroStderrTailCapacity,
}

/// Typed renderer configuration sections.
#[derive(Clone, Debug, PartialEq)]
pub struct RendererConfigs {
    pub(super) wgpu: WgpuRendererConfig,
    pub(super) blenderproc: BlenderProcRendererConfig,
}

impl RendererConfigs {
    /// Returns wgpu options.
    #[must_use]
    pub const fn wgpu(&self) -> WgpuRendererConfig {
        self.wgpu
    }
    /// Returns `BlenderProc` worker options.
    #[must_use]
    pub const fn blenderproc(&self) -> &BlenderProcRendererConfig {
        &self.blenderproc
    }

    pub(super) fn try_from_wire(wire: RenderersWire) -> Result<Self, RunConfigValidationError> {
        Ok(Self {
            wgpu: WgpuRendererConfig::try_from_wire(wire.wgpu)?,
            blenderproc: BlenderProcRendererConfig::try_from_wire(wire.blenderproc)?,
        })
    }
}

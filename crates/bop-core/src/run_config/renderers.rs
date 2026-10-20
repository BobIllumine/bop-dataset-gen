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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WgpuRendererConfig {
    pub(super) device_preference: WgpuDevicePreference,
    pub(super) adapter_preference: WgpuAdapterPreference,
    pub(super) shadow_map_size: u32,
    pub(super) rgb_fxaa: bool,
    pub(super) ambient_occlusion_radius_m: f32,
    pub(super) screen_space_reflections: bool,
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
            ambient_occlusion_radius_m: Self::DEFAULT_AMBIENT_OCCLUSION_RADIUS_M,
            screen_space_reflections: true,
        })
    }

    /// Turns screen-space reflections on or off (on by default).
    #[must_use]
    pub const fn with_screen_space_reflections(mut self, enabled: bool) -> Self {
        self.screen_space_reflections = enabled;
        self
    }

    /// Reports whether glossy surfaces reflect what the camera sees.
    #[must_use]
    pub const fn screen_space_reflections(self) -> bool {
        self.screen_space_reflections
    }

    /// Ambient-occlusion search radius used when none is configured.
    pub const DEFAULT_AMBIENT_OCCLUSION_RADIUS_M: f32 = 0.7;

    /// Replaces the ambient-occlusion radius; zero disables occlusion.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidWgpuRendererConfigError::InvalidAmbientOcclusionRadius`]
    /// for a negative or non-finite radius.
    pub fn try_with_ambient_occlusion_radius(
        mut self,
        radius_m: f32,
    ) -> Result<Self, InvalidWgpuRendererConfigError> {
        if !radius_m.is_finite() || radius_m < 0.0 {
            return Err(InvalidWgpuRendererConfigError::InvalidAmbientOcclusionRadius);
        }
        self.ambient_occlusion_radius_m = radius_m;
        Ok(self)
    }

    /// Returns the ambient-occlusion search radius in metres; zero disables it.
    #[must_use]
    pub const fn ambient_occlusion_radius_m(self) -> f32 {
        self.ambient_occlusion_radius_m
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
        nonnegative_f32(
            wire.ambient_occlusion_radius_m,
            ConfigScalarField::WgpuAmbientOcclusionRadius,
        )?;
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
            ambient_occlusion_radius_m: wire.ambient_occlusion_radius_m,
            screen_space_reflections: wire.screen_space_reflections,
        })
    }
}

/// Error returned when programmatic `wgpu` renderer options are invalid.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidWgpuRendererConfigError {
    /// Shadow maps require a nonzero square resolution.
    #[error("shadow_map_size must be nonzero")]
    ZeroShadowMapSize,
    /// Ambient-occlusion radius is negative or not finite.
    #[error("ambient_occlusion_radius_m must be finite and nonnegative")]
    InvalidAmbientOcclusionRadius,
}

/// The denoiser Cycles applies to the RGB channel. Depth and instance labels
/// are never denoised.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlenderProcDenoiser {
    /// No denoising.
    None,
    /// Intel Open Image Denoise, on the GPU where Blender supports it.
    Oidn,
    /// NVIDIA `OptiX`; requires an `OptiX` device.
    Optix,
}

impl BlenderProcDenoiser {
    /// The name the worker protocol and configuration use.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Oidn => "oidn",
            Self::Optix => "optix",
        }
    }
}

/// Cycles path-tracing quality: the main cost of a `BlenderProc` frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlenderProcSampling {
    samples: u32,
    noise_threshold: f32,
    denoiser: BlenderProcDenoiser,
    glossy_bounces: u32,
}

impl BlenderProcSampling {
    /// 128 samples per pixel, adaptive noise threshold 0.01, OIDN, one
    /// glossy bounce.
    pub const DEFAULT: Self = Self {
        samples: 128,
        noise_threshold: 0.01,
        denoiser: BlenderProcDenoiser::Oidn,
        glossy_bounces: 1,
    };

    /// Creates checked sampling options.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidBlenderProcRendererConfigError::ZeroSamples`] for zero
    /// samples and [`InvalidBlenderProcRendererConfigError::InvalidNoiseThreshold`]
    /// for a negative or non-finite threshold. A threshold of zero disables
    /// adaptive sampling.
    pub fn try_new(
        samples: u32,
        noise_threshold: f32,
        denoiser: BlenderProcDenoiser,
    ) -> Result<Self, InvalidBlenderProcRendererConfigError> {
        if samples == 0 {
            return Err(InvalidBlenderProcRendererConfigError::ZeroSamples);
        }
        if !noise_threshold.is_finite() || noise_threshold < 0.0 {
            return Err(InvalidBlenderProcRendererConfigError::InvalidNoiseThreshold);
        }
        Ok(Self {
            samples,
            noise_threshold,
            denoiser,
            glossy_bounces: Self::DEFAULT.glossy_bounces,
        })
    }

    /// Replaces how many times light may bounce off glossy surfaces; 0 makes
    /// metals reflect nothing but direct light.
    #[must_use]
    pub const fn with_glossy_bounces(mut self, glossy_bounces: u32) -> Self {
        self.glossy_bounces = glossy_bounces;
        self
    }

    /// Returns the glossy bounce limit.
    #[must_use]
    pub const fn glossy_bounces(self) -> u32 {
        self.glossy_bounces
    }

    /// Returns the nonzero maximum samples per pixel.
    #[must_use]
    pub const fn samples(self) -> u32 {
        self.samples
    }
    /// Returns the adaptive-sampling noise threshold; zero disables it.
    #[must_use]
    pub const fn noise_threshold(self) -> f32 {
        self.noise_threshold
    }
    /// Returns the RGB denoiser.
    #[must_use]
    pub const fn denoiser(self) -> BlenderProcDenoiser {
        self.denoiser
    }
}

impl Default for BlenderProcSampling {
    fn default() -> Self {
        Self::DEFAULT
    }
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
    pub(super) sampling: BlenderProcSampling,
}

impl BlenderProcRendererConfig {
    /// Constructs checked programmatic worker options with
    /// [`BlenderProcSampling::DEFAULT`]; see [`Self::with_sampling`].
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
            sampling: BlenderProcSampling::DEFAULT,
        })
    }

    /// Replaces the sampling options.
    #[must_use]
    pub const fn with_sampling(mut self, sampling: BlenderProcSampling) -> Self {
        self.sampling = sampling;
        self
    }

    /// Returns the Cycles sampling options.
    #[must_use]
    pub const fn sampling(&self) -> BlenderProcSampling {
        self.sampling
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
        nonzero(wire.samples, ConfigCountField::BlenderProcSamples)?;
        nonnegative_f32(
            wire.noise_threshold,
            ConfigScalarField::BlenderProcNoiseThreshold,
        )?;
        Ok(Self {
            executable_path,
            worker_path,
            device_selection: wire.device_selection,
            handshake_timeout_s: wire.handshake_timeout_s,
            request_timeout_s: wire.request_timeout_s,
            stderr_tail_capacity: wire.stderr_tail_capacity,
            sampling: BlenderProcSampling {
                samples: wire.samples,
                noise_threshold: wire.noise_threshold,
                denoiser: match wire.denoiser {
                    BlenderProcDenoiserWire::None => BlenderProcDenoiser::None,
                    BlenderProcDenoiserWire::Oidn => BlenderProcDenoiser::Oidn,
                    BlenderProcDenoiserWire::Optix => BlenderProcDenoiser::Optix,
                },
                glossy_bounces: wire.glossy_bounces,
            },
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
    /// Sample count is zero.
    #[error("samples must be nonzero")]
    ZeroSamples,
    /// Noise threshold is negative or not finite.
    #[error("noise_threshold must be finite and nonnegative")]
    InvalidNoiseThreshold,
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

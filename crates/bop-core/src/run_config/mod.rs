use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use thiserror::Error;

use self::wire::{
    AmbientWire, AppearanceWire, AssetWire, BinEntryWire, BinRenderMaterialWire, BinWire,
    BlenderProcDenoiserWire, BlenderProcRendererWire, CameraPoseWire, CameraProjectionWire,
    CameraWire, DEFAULT_ANGULAR_SLEEP_THRESHOLD_RAD_S, DEFAULT_ESCAPE_MARGIN_M,
    DEFAULT_FIXED_TIMESTEP_S, DEFAULT_LINEAR_SLEEP_THRESHOLD_MPS, DEFAULT_MAX_STEPS,
    DEFAULT_MAXIMUM_PENETRATION_M, DEFAULT_MIN_STEPS, DEFAULT_PLACEMENT_ATTEMPT_LIMIT,
    DEFAULT_REQUIRED_STABLE_CHECKS, DEFAULT_SCENE_ATTEMPT_LIMIT, DEFAULT_STABILITY_CHECK_INTERVAL,
    DirectionalLightWire, DistributionWire, ImageSizeWire, OutputWire, PbrMaterialWire,
    PhysicalPropertyDistributionsWire, PhysicsWire, PngCompressionWire, PointLightWire,
    PopulationWire, RenderersWire, RunConfigWire, RunWire, ShellCameraPoseWire, SpawnVolumeWire,
    SurfacePhysicalPropertiesWire, TransformWire, VhacdWire, WgpuAdapterPreferenceWire,
    WgpuDevicePreferenceWire, WgpuRendererWire,
};
use crate::{
    AmbientLight, AssetId, BinRenderMaterial, CameraId, CameraIntrinsics, DirectionalLight,
    Distribution, DistributionView, ImageSize, InvalidDistributionError, InvalidWeightedValueError,
    LinearRgb, Meters, NonNegativeFinite, PointLight, PositiveMeters, RenderChannels,
    RigidTransform, SrgbColor, WeightedValue,
};

mod cameras;
mod errors;
mod renderers;
mod scene;
mod validation;
mod wire;

pub use cameras::*;
pub use errors::*;
pub use renderers::*;
pub use scene::*;
#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the parent's imports and helpers"
)]
use validation::*;
#[cfg(feature = "schema")]
pub(crate) use wire::json_schema;

/// A parsed v1 run configuration whose nested values have all been validated.
///
/// This type is not deserializable directly. [`Self::from_toml`] first reads a
/// private unchecked wire representation and only constructs this value after
/// every local and cross-field invariant has passed.
#[derive(Clone, Debug, PartialEq)]
pub struct RunConfig {
    plan: RunPlan,
}

impl RunConfig {
    /// Parses strict TOML and validates it into a deterministic execution plan.
    ///
    /// This operation is pure: it does not inspect paths, sample distributions,
    /// derive seeds, initialize renderers, or allocate physics state.
    ///
    /// # Errors
    ///
    /// Returns [`RunConfigError::Toml`] for TOML syntax and shape failures,
    /// including missing or unknown fields. Returns
    /// [`RunConfigError::Validation`] for typed semantic failures.
    pub fn from_toml(input: &str) -> Result<Self, RunConfigError> {
        let wire = toml::from_str::<RunConfigWire>(input)?;
        Ok(Self {
            plan: RunPlan::try_from_wire(wire)?,
        })
    }

    /// Borrows the fully validated, resolved execution plan.
    #[must_use]
    pub const fn plan(&self) -> &RunPlan {
        &self.plan
    }

    /// Consumes the parsed configuration and returns its execution plan.
    #[must_use]
    pub fn into_plan(self) -> RunPlan {
        self.plan
    }
}

/// The sole validated aggregate consumed by later execution milestones.
///
/// Asset and camera slices are sorted by stable ID. Light slices retain their
/// configured semantic order. Every documented default is resolved before
/// this value is constructed.
#[derive(Clone, Debug, PartialEq)]
pub struct RunPlan {
    run: RunSettings,
    bins: Vec<BinPlan>,
    assets: Vec<AssetConfig>,
    physics: PhysicsConfig,
    cameras: Vec<CameraConfig>,
    appearance: AppearanceConfig,
    renderers: RendererConfigs,
    output_channels: RenderChannels,
    png_compression: crate::PngCompression,
}

impl RunPlan {
    /// Returns run-control settings with defaults resolved.
    #[must_use]
    pub const fn run(&self) -> &RunSettings {
        &self.run
    }

    /// Returns the bins in configuration order, each with its population; never empty.
    #[must_use]
    pub fn bins(&self) -> &[BinPlan] {
        &self.bins
    }

    /// Returns the first bin's geometry and physical configuration.
    ///
    /// This is the only bin of a configuration written with `[bin]`; code that
    /// generates or renders scenes must use [`Self::bins`].
    #[must_use]
    pub fn bin(&self) -> &BinConfig {
        self.bins[0].bin()
    }

    /// Returns assets sorted by [`AssetId`].
    #[must_use]
    pub fn assets(&self) -> &[AssetConfig] {
        &self.assets
    }

    /// Returns the first bin's population; see [`Self::bin`].
    #[must_use]
    pub fn population(&self) -> &PopulationConfig {
        self.bins[0].population()
    }

    /// Returns physics settings with defaults resolved.
    #[must_use]
    pub const fn physics(&self) -> &PhysicsConfig {
        &self.physics
    }

    /// Returns cameras sorted by [`CameraId`].
    #[must_use]
    pub fn cameras(&self) -> &[CameraConfig] {
        &self.cameras
    }

    /// Returns rendering appearance inputs.
    #[must_use]
    pub const fn appearance(&self) -> &AppearanceConfig {
        &self.appearance
    }

    /// Returns renderer option objects without initializing a renderer.
    #[must_use]
    pub const fn renderers(&self) -> &RendererConfigs {
        &self.renderers
    }

    /// Returns the checked nonempty output channel set.
    #[must_use]
    pub const fn output_channels(&self) -> RenderChannels {
        self.output_channels
    }

    /// Returns how RGB and instance PNGs are compressed.
    #[must_use]
    pub const fn png_compression(&self) -> crate::PngCompression {
        self.png_compression
    }

    fn try_from_wire(wire: RunConfigWire) -> Result<Self, RunConfigValidationError> {
        let run = RunSettings::try_from_wire(wire.run)?;
        let assets = validate_assets(wire.assets)?;
        let bins = validate_bins(wire.bin, wire.population, wire.bins, &assets)?;
        let physics = PhysicsConfig::try_from_wire(wire.physics)?;
        let cameras = validate_cameras(wire.cameras)?;
        let appearance = AppearanceConfig::try_from_wire(wire.appearance)?;
        let renderers = RendererConfigs::try_from_wire(wire.renderers)?;
        let maximum_objects = bins
            .iter()
            .map(|bin| maximum_count(bin.population().object_count()))
            .fold(0_u32, u32::saturating_add);
        let png_compression = match wire.output.png_compression {
            PngCompressionWire::Fast => crate::PngCompression::Fast,
            PngCompressionWire::Balanced => crate::PngCompression::Balanced,
            PngCompressionWire::High => crate::PngCompression::High,
        };
        let output_channels = validate_output(wire.output, maximum_objects)?;

        Ok(Self {
            run,
            bins,
            assets,
            physics,
            cameras,
            appearance,
            renderers,
            output_channels,
            png_compression,
        })
    }
}

/// Immutable run-control values.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSettings {
    master_seed: u64,
    valid_scene_count: u32,
    scene_attempt_limit: u32,
    placement_attempt_limit_per_object: u32,
    simulation_workers: u32,
    renderer_workers: u32,
    queue_capacity: u32,
    output_path: PathBuf,
    cache_path: PathBuf,
}

impl RunSettings {
    /// Returns the configured master seed without deriving child seeds.
    #[must_use]
    pub const fn master_seed(&self) -> u64 {
        self.master_seed
    }
    /// Returns the requested number of valid scenes.
    #[must_use]
    pub const fn valid_scene_count(&self) -> u32 {
        self.valid_scene_count
    }
    /// Returns the maximum attempts allowed for each requested scene.
    #[must_use]
    pub const fn scene_attempt_limit(&self) -> u32 {
        self.scene_attempt_limit
    }
    /// Returns the placement-attempt limit for each object.
    #[must_use]
    pub const fn placement_attempt_limit_per_object(&self) -> u32 {
        self.placement_attempt_limit_per_object
    }
    /// Returns the simulation worker count.
    #[must_use]
    pub const fn simulation_workers(&self) -> u32 {
        self.simulation_workers
    }
    /// Returns the renderer worker count.
    #[must_use]
    pub const fn renderer_workers(&self) -> u32 {
        self.renderer_workers
    }
    /// Returns the bounded queue capacity.
    #[must_use]
    pub const fn queue_capacity(&self) -> u32 {
        self.queue_capacity
    }
    /// Returns the unchecked-for-existence output path.
    #[must_use]
    pub fn output_path(&self) -> &Path {
        &self.output_path
    }
    /// Returns the unchecked-for-existence cache path.
    #[must_use]
    pub fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    fn try_from_wire(wire: RunWire) -> Result<Self, RunConfigValidationError> {
        nonzero(wire.valid_scene_count, ConfigCountField::ValidSceneCount)?;
        let scene_attempt_limit = wire
            .scene_attempt_limit
            .unwrap_or(DEFAULT_SCENE_ATTEMPT_LIMIT);
        nonzero(scene_attempt_limit, ConfigCountField::SceneAttemptLimit)?;
        let placement_attempt_limit_per_object = wire
            .placement_attempt_limit_per_object
            .unwrap_or(DEFAULT_PLACEMENT_ATTEMPT_LIMIT);
        nonzero(
            placement_attempt_limit_per_object,
            ConfigCountField::PlacementAttemptLimitPerObject,
        )?;
        nonzero(wire.simulation_workers, ConfigCountField::SimulationWorkers)?;
        nonzero(wire.renderer_workers, ConfigCountField::RendererWorkers)?;
        nonzero(wire.queue_capacity, ConfigCountField::QueueCapacity)?;
        let output_path = checked_path(wire.output_path, ConfigPathField::Output, None)?;
        let cache_path = checked_path(wire.cache_path, ConfigPathField::Cache, None)?;

        Ok(Self {
            master_seed: wire.master_seed,
            valid_scene_count: wire.valid_scene_count,
            scene_attempt_limit,
            placement_attempt_limit_per_object,
            simulation_workers: wire.simulation_workers,
            renderer_workers: wire.renderer_workers,
            queue_capacity: wire.queue_capacity,
            output_path,
            cache_path,
        })
    }
}

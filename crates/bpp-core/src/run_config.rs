use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::run_config_wire::{
    AmbientWire, AppearanceWire, AssetWire, BinWire, BlenderProcRendererWire, CameraPoseWire,
    CameraProjectionWire, CameraWire, DEFAULT_ANGULAR_SLEEP_THRESHOLD_RAD_S,
    DEFAULT_ESCAPE_MARGIN_M, DEFAULT_FIXED_TIMESTEP_S, DEFAULT_LINEAR_SLEEP_THRESHOLD_MPS,
    DEFAULT_MAX_STEPS, DEFAULT_MAXIMUM_PENETRATION_M, DEFAULT_MIN_STEPS,
    DEFAULT_PLACEMENT_ATTEMPT_LIMIT, DEFAULT_REQUIRED_STABLE_CHECKS, DEFAULT_SCENE_ATTEMPT_LIMIT,
    DEFAULT_STABILITY_CHECK_INTERVAL, DirectionalLightWire, DistributionWire, ImageSizeWire,
    OutputWire, PbrMaterialWire, PhysicalPropertyDistributionsWire, PhysicsWire, PointLightWire,
    PopulationWire, RenderersWire, RunConfigWire, RunWire, ShellCameraPoseWire, SpawnVolumeWire,
    SurfacePhysicalPropertiesWire, TransformWire, VhacdWire, WgpuAdapterPreferenceWire,
    WgpuDevicePreferenceWire, WgpuRendererWire,
};
use crate::{
    AmbientLight, AssetId, CameraId, CameraIntrinsics, DirectionalLight, Distribution,
    DistributionView, ImageSize, InvalidDistributionError, InvalidWeightedValueError, LinearRgb,
    Meters, NonNegativeFinite, PointLight, PositiveMeters, RenderChannels, RigidTransform,
    SrgbColor, WeightedValue,
};

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
    bin: BinConfig,
    assets: Vec<AssetConfig>,
    population: PopulationConfig,
    physics: PhysicsConfig,
    cameras: Vec<CameraConfig>,
    appearance: AppearanceConfig,
    renderers: RendererConfigs,
    output_channels: RenderChannels,
}

impl RunPlan {
    /// Returns run-control settings with defaults resolved.
    #[must_use]
    pub const fn run(&self) -> &RunSettings {
        &self.run
    }

    /// Returns the checked bin geometry and physical configuration.
    #[must_use]
    pub const fn bin(&self) -> &BinConfig {
        &self.bin
    }

    /// Returns assets sorted by [`AssetId`].
    #[must_use]
    pub fn assets(&self) -> &[AssetConfig] {
        &self.assets
    }

    /// Returns checked population distributions and spawn volume.
    #[must_use]
    pub const fn population(&self) -> &PopulationConfig {
        &self.population
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

    fn try_from_wire(wire: RunConfigWire) -> Result<Self, RunConfigValidationError> {
        let run = RunSettings::try_from_wire(wire.run)?;
        let bin = BinConfig::try_from_wire(wire.bin)?;
        let assets = validate_assets(wire.assets)?;
        let population = PopulationConfig::try_from_wire(wire.population, &assets, &bin)?;
        let physics = PhysicsConfig::try_from_wire(wire.physics)?;
        let cameras = validate_cameras(wire.cameras)?;
        let appearance = AppearanceConfig::try_from_wire(wire.appearance)?;
        let renderers = RendererConfigs::try_from_wire(wire.renderers)?;
        let output_channels =
            validate_output(wire.output, maximum_count(population.object_count()))?;

        Ok(Self {
            run,
            bin,
            assets,
            population,
            physics,
            cameras,
            appearance,
            renderers,
            output_channels,
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

/// Checked static contact properties for the bin interior.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePhysicalProperties {
    friction: f32,
    restitution: f32,
}

impl SurfacePhysicalProperties {
    /// Returns the finite nonnegative friction coefficient.
    #[must_use]
    pub const fn friction(self) -> f32 {
        self.friction
    }
    /// Returns the restitution coefficient in the inclusive range `[0, 1]`.
    #[must_use]
    pub const fn restitution(self) -> f32 {
        self.restitution
    }
}

/// Checked bin geometry, pose, material label, and contact properties.
#[derive(Clone, Debug, PartialEq)]
pub struct BinConfig {
    inner_dimensions_m: [PositiveMeters; 3],
    floor_thickness_m: PositiveMeters,
    wall_thickness_m: PositiveMeters,
    world_from_bin: RigidTransform,
    material_label: String,
    physical_properties: SurfacePhysicalProperties,
}

impl BinConfig {
    /// Returns positive inner `[x, y, z]` dimensions in metres.
    #[must_use]
    pub const fn inner_dimensions_m(&self) -> [PositiveMeters; 3] {
        self.inner_dimensions_m
    }
    /// Returns the positive floor thickness in metres.
    #[must_use]
    pub const fn floor_thickness_m(&self) -> PositiveMeters {
        self.floor_thickness_m
    }
    /// Returns the positive wall thickness in metres.
    #[must_use]
    pub const fn wall_thickness_m(&self) -> PositiveMeters {
        self.wall_thickness_m
    }
    /// Returns the transform from bin-local coordinates to world coordinates.
    #[must_use]
    pub const fn world_from_bin(&self) -> RigidTransform {
        self.world_from_bin
    }
    /// Returns the nonblank material label.
    #[must_use]
    pub fn material_label(&self) -> &str {
        &self.material_label
    }
    /// Returns the checked bin contact properties.
    #[must_use]
    pub const fn physical_properties(&self) -> SurfacePhysicalProperties {
        self.physical_properties
    }

    fn try_from_wire(wire: BinWire) -> Result<Self, RunConfigValidationError> {
        let mut dimensions = [PositiveMeters::try_new(1.0).expect("one metre is positive"); 3];
        for (index, value) in wire.inner_dimensions_m.into_iter().enumerate() {
            dimensions[index] = PositiveMeters::try_new(value).map_err(|_| {
                RunConfigValidationError::InvalidPositiveScalar {
                    field: match index {
                        0 => ConfigScalarField::BinInnerX,
                        1 => ConfigScalarField::BinInnerY,
                        _ => ConfigScalarField::BinInnerZ,
                    },
                    value: f64::from(value),
                }
            })?;
        }
        let floor_thickness_m =
            positive_meters(wire.floor_thickness_m, ConfigScalarField::BinFloorThickness)?;
        let wall_thickness_m =
            positive_meters(wire.wall_thickness_m, ConfigScalarField::BinWallThickness)?;
        let world_from_bin =
            checked_transform(wire.world_from_bin, ConfigTransformField::Bin, None, None)?;
        if wire.material_label.trim().is_empty() {
            return Err(RunConfigValidationError::InvalidText {
                field: ConfigTextField::BinMaterialLabel,
            });
        }
        let physical_properties =
            SurfacePhysicalProperties::try_from_wire(wire.physical_properties)?;
        Ok(Self {
            inner_dimensions_m: dimensions,
            floor_thickness_m,
            wall_thickness_m,
            world_from_bin,
            material_label: wire.material_label,
            physical_properties,
        })
    }
}

impl SurfacePhysicalProperties {
    fn try_from_wire(
        wire: SurfacePhysicalPropertiesWire,
    ) -> Result<Self, RunConfigValidationError> {
        nonnegative_f32(wire.friction, ConfigScalarField::BinFriction)?;
        unit_interval(wire.restitution, ConfigScalarField::BinRestitution)?;
        Ok(Self {
            friction: canonicalize_zero(wire.friction),
            restitution: canonicalize_zero(wire.restitution),
        })
    }
}

/// Checked physical-property distributions for one asset.
#[derive(Clone, Debug, PartialEq)]
pub struct PhysicalPropertyDistributions {
    mass_kg: Distribution<f32>,
    friction: Distribution<f32>,
    restitution: Distribution<f32>,
}

impl PhysicalPropertyDistributions {
    /// Returns the strictly-positive mass distribution in kilograms.
    #[must_use]
    pub const fn mass_kg(&self) -> &Distribution<f32> {
        &self.mass_kg
    }
    /// Returns the nonnegative friction distribution.
    #[must_use]
    pub const fn friction(&self) -> &Distribution<f32> {
        &self.friction
    }
    /// Returns the restitution distribution constrained to `[0, 1]`.
    #[must_use]
    pub const fn restitution(&self) -> &Distribution<f32> {
        &self.restitution
    }

    fn try_from_wire(
        wire: PhysicalPropertyDistributionsWire,
        asset_id: &AssetId,
    ) -> Result<Self, RunConfigValidationError> {
        Ok(Self {
            mass_kg: checked_float_distribution(
                wire.mass_kg,
                DistributionField::MassKg,
                Some(asset_id),
                FloatDomain::Positive,
            )?,
            friction: checked_float_distribution(
                wire.friction,
                DistributionField::Friction,
                Some(asset_id),
                FloatDomain::NonNegative,
            )?,
            restitution: checked_float_distribution(
                wire.restitution,
                DistributionField::Restitution,
                Some(asset_id),
                FloatDomain::UnitInterval,
            )?,
        })
    }
}

/// Checked PBR material values or distributions.
#[derive(Clone, Debug, PartialEq)]
pub struct PbrMaterialConfig {
    base_color_srgb: SrgbColor,
    metallic: Distribution<f32>,
    roughness: Distribution<f32>,
}

impl PbrMaterialConfig {
    /// Returns the checked base color.
    #[must_use]
    pub const fn base_color_srgb(&self) -> SrgbColor {
        self.base_color_srgb
    }
    /// Returns metallic values constrained to `[0, 1]`.
    #[must_use]
    pub const fn metallic(&self) -> &Distribution<f32> {
        &self.metallic
    }
    /// Returns roughness values constrained to `[0, 1]`.
    #[must_use]
    pub const fn roughness(&self) -> &Distribution<f32> {
        &self.roughness
    }

    fn try_from_wire(
        wire: PbrMaterialWire,
        asset_id: Option<&AssetId>,
    ) -> Result<Self, RunConfigValidationError> {
        let base_color_srgb = SrgbColor::try_new(wire.base_color_srgb).map_err(|_| {
            RunConfigValidationError::InvalidAppearance {
                field: AppearanceField::BaseColor,
                index: None,
            }
        })?;
        Ok(Self {
            base_color_srgb,
            metallic: checked_float_distribution(
                wire.metallic,
                DistributionField::Metallic,
                asset_id,
                FloatDomain::UnitInterval,
            )?,
            roughness: checked_float_distribution(
                wire.roughness,
                DistributionField::Roughness,
                asset_id,
                FloatDomain::UnitInterval,
            )?,
        })
    }
}

/// Checked V-HACD decomposition parameters for one asset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VhacdParameters {
    resolution: u32,
    max_convex_hulls: u32,
    concavity: f32,
    plane_downsampling: u32,
    convex_hull_downsampling: u32,
}

impl VhacdParameters {
    /// Returns the voxel resolution.
    #[must_use]
    pub const fn resolution(self) -> u32 {
        self.resolution
    }
    /// Returns the maximum convex hull count.
    #[must_use]
    pub const fn max_convex_hulls(self) -> u32 {
        self.max_convex_hulls
    }
    /// Returns the finite nonnegative concavity threshold.
    #[must_use]
    pub const fn concavity(self) -> f32 {
        self.concavity
    }
    /// Returns the plane downsampling factor.
    #[must_use]
    pub const fn plane_downsampling(self) -> u32 {
        self.plane_downsampling
    }
    /// Returns the convex-hull downsampling factor.
    #[must_use]
    pub const fn convex_hull_downsampling(self) -> u32 {
        self.convex_hull_downsampling
    }

    fn try_from_wire(wire: VhacdWire) -> Result<Self, RunConfigValidationError> {
        nonzero(wire.resolution, ConfigCountField::VhacdResolution)?;
        nonzero(wire.max_convex_hulls, ConfigCountField::VhacdMaxConvexHulls)?;
        nonnegative_f32(wire.concavity, ConfigScalarField::VhacdConcavity)?;
        nonzero(
            wire.plane_downsampling,
            ConfigCountField::VhacdPlaneDownsampling,
        )?;
        nonzero(
            wire.convex_hull_downsampling,
            ConfigCountField::VhacdConvexHullDownsampling,
        )?;
        Ok(Self {
            resolution: wire.resolution,
            max_convex_hulls: wire.max_convex_hulls,
            concavity: canonicalize_zero(wire.concavity),
            plane_downsampling: wire.plane_downsampling,
            convex_hull_downsampling: wire.convex_hull_downsampling,
        })
    }
}

/// Immutable checked asset input.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetConfig {
    id: AssetId,
    source_path: PathBuf,
    source_unit_scale_m: PositiveMeters,
    model_transform: RigidTransform,
    mixture_weight: f64,
    physical_properties: PhysicalPropertyDistributions,
    visual_override: Option<PbrMaterialConfig>,
    vhacd: VhacdParameters,
}

impl AssetConfig {
    /// Returns the stable logical asset ID.
    #[must_use]
    pub const fn id(&self) -> &AssetId {
        &self.id
    }
    /// Returns the nonempty source path without accessing the filesystem.
    #[must_use]
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }
    /// Returns the positive source-unit scale in metres.
    #[must_use]
    pub const fn source_unit_scale_m(&self) -> PositiveMeters {
        self.source_unit_scale_m
    }
    /// Returns the checked transform from metre-scaled source-model coordinates
    /// into canonical right-handed Z-up coordinates.
    ///
    /// Its translation is measured in metres and is applied after the source
    /// unit scale. No implicit recentering or axis inference is performed.
    #[must_use]
    pub const fn model_transform(&self) -> RigidTransform {
        self.model_transform
    }
    /// Returns the finite nonnegative catalog mixture weight.
    #[must_use]
    pub const fn mixture_weight(&self) -> f64 {
        self.mixture_weight
    }
    /// Returns physical-property distributions.
    #[must_use]
    pub const fn physical_properties(&self) -> &PhysicalPropertyDistributions {
        &self.physical_properties
    }
    /// Returns an optional checked visual override.
    #[must_use]
    pub const fn visual_override(&self) -> Option<&PbrMaterialConfig> {
        self.visual_override.as_ref()
    }
    /// Returns V-HACD settings.
    #[must_use]
    pub const fn vhacd(&self) -> VhacdParameters {
        self.vhacd
    }

    fn try_from_wire(wire: AssetWire) -> Result<Self, RunConfigValidationError> {
        let id = AssetId::new(wire.id.clone())
            .ok_or(RunConfigValidationError::InvalidAssetId { value: wire.id })?;
        let source_path = checked_path(wire.source_path, ConfigPathField::AssetSource, Some(&id))?;
        let source_unit_scale_m = positive_meters(
            wire.source_unit_scale_m,
            ConfigScalarField::AssetSourceUnitScale,
        )?;
        let model_transform = checked_transform(
            wire.model_transform,
            ConfigTransformField::AssetModel,
            Some(&id),
            None,
        )?;
        if !wire.mixture_weight.is_finite() || wire.mixture_weight < 0.0 {
            return Err(RunConfigValidationError::InvalidAssetMixtureWeight {
                asset_id: id,
                value: wire.mixture_weight,
            });
        }
        let physical_properties =
            PhysicalPropertyDistributions::try_from_wire(wire.physical_properties, &id)?;
        let visual_override = wire
            .visual_override
            .map(|value| PbrMaterialConfig::try_from_wire(value, Some(&id)))
            .transpose()?;
        let vhacd = VhacdParameters::try_from_wire(wire.vhacd)?;
        Ok(Self {
            id,
            source_path,
            source_unit_scale_m,
            model_transform,
            mixture_weight: canonicalize_zero_f64(wire.mixture_weight),
            physical_properties,
            visual_override,
            vhacd,
        })
    }
}

/// An axis in bin-local XYZ coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    /// Bin-local X axis.
    X,
    /// Bin-local Y axis.
    Y,
    /// Bin-local Z axis.
    Z,
}

/// A finite, nonempty spawn volume in bin-local coordinates.
///
/// The v1 bin-local origin is centered on the inner floor and +Z is up. For
/// inner dimensions `[X, Y, Z]`, closed interior bounds are
/// `[-X/2, X/2] × [-Y/2, Y/2] × [0, Z]`. Spawn endpoints may touch those
/// boundaries, while every spawn axis must satisfy `min < max`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpawnVolume {
    min_m: [f32; 3],
    max_m: [f32; 3],
}

impl SpawnVolume {
    /// Returns finite inclusive lower endpoints in bin-local metres.
    #[must_use]
    pub const fn min_m(self) -> [f32; 3] {
        self.min_m
    }
    /// Returns finite inclusive upper endpoints in bin-local metres.
    #[must_use]
    pub const fn max_m(self) -> [f32; 3] {
        self.max_m
    }

    fn try_from_wire(
        wire: SpawnVolumeWire,
        bin: &BinConfig,
    ) -> Result<Self, RunConfigValidationError> {
        let dimensions = bin.inner_dimensions_m().map(PositiveMeters::get);
        let interior_min = [-dimensions[0] / 2.0, -dimensions[1] / 2.0, 0.0];
        let interior_max = [dimensions[0] / 2.0, dimensions[1] / 2.0, dimensions[2]];
        let min_m = wire.min_m.map(canonicalize_zero);
        let max_m = wire.max_m.map(canonicalize_zero);
        for index in 0..3 {
            let axis = axis(index);
            if !min_m[index].is_finite()
                || !max_m[index].is_finite()
                || min_m[index] >= max_m[index]
            {
                return Err(RunConfigValidationError::InvalidSpawnAxis { axis });
            }
            if min_m[index] < interior_min[index] || max_m[index] > interior_max[index] {
                return Err(RunConfigValidationError::SpawnOutsideBin { axis });
            }
        }
        Ok(Self { min_m, max_m })
    }
}

/// Checked population inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct PopulationConfig {
    object_count: Distribution<u32>,
    asset_mixture: Distribution<AssetId>,
    spawn_volume: SpawnVolume,
}

impl PopulationConfig {
    /// Returns a distribution whose every possible count is positive.
    #[must_use]
    pub const fn object_count(&self) -> &Distribution<u32> {
        &self.object_count
    }
    /// Returns a nonempty asset mixture referencing only configured assets.
    #[must_use]
    pub const fn asset_mixture(&self) -> &Distribution<AssetId> {
        &self.asset_mixture
    }
    /// Returns the checked bin-local spawn volume.
    #[must_use]
    pub const fn spawn_volume(&self) -> SpawnVolume {
        self.spawn_volume
    }

    fn try_from_wire(
        wire: PopulationWire,
        assets: &[AssetConfig],
        bin: &BinConfig,
    ) -> Result<Self, RunConfigValidationError> {
        let object_count = checked_object_count(wire.object_count)?;
        let asset_mixture = checked_asset_distribution(wire.asset_mixture)?;
        let configured_ids = assets.iter().map(AssetConfig::id).collect::<BTreeSet<_>>();
        for asset_id in distribution_values(&asset_mixture) {
            if !configured_ids.contains(asset_id) {
                return Err(RunConfigValidationError::UnknownAssetReference {
                    asset_id: asset_id.clone(),
                });
            }
        }
        let spawn_volume = SpawnVolume::try_from_wire(wire.spawn_volume, bin)?;
        Ok(Self {
            object_count,
            asset_mixture,
            spawn_volume,
        })
    }
}

/// Resolved deterministic physics settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicsConfig {
    fixed_timestep_s: f32,
    substeps: u32,
    solver_iterations: u32,
    min_steps: u32,
    max_steps: u32,
    stability_check_interval: u32,
    linear_sleep_threshold_mps: f32,
    angular_sleep_threshold_rad_s: f32,
    required_stable_checks: u32,
    escape_margin_m: f32,
    maximum_penetration_m: f32,
}

impl PhysicsConfig {
    /// Returns the positive fixed timestep in seconds.
    #[must_use]
    pub const fn fixed_timestep_s(self) -> f32 {
        self.fixed_timestep_s
    }
    /// Returns the nonzero substep count.
    #[must_use]
    pub const fn substeps(self) -> u32 {
        self.substeps
    }
    /// Returns the nonzero solver iteration count.
    #[must_use]
    pub const fn solver_iterations(self) -> u32 {
        self.solver_iterations
    }
    /// Returns the nonzero minimum step count.
    #[must_use]
    pub const fn min_steps(self) -> u32 {
        self.min_steps
    }
    /// Returns the nonzero maximum step count.
    #[must_use]
    pub const fn max_steps(self) -> u32 {
        self.max_steps
    }
    /// Returns the nonzero stability-check interval in steps.
    #[must_use]
    pub const fn stability_check_interval(self) -> u32 {
        self.stability_check_interval
    }
    /// Returns the finite nonnegative linear sleep threshold in metres per second.
    #[must_use]
    pub const fn linear_sleep_threshold_mps(self) -> f32 {
        self.linear_sleep_threshold_mps
    }
    /// Returns the finite nonnegative angular sleep threshold in radians per second.
    #[must_use]
    pub const fn angular_sleep_threshold_rad_s(self) -> f32 {
        self.angular_sleep_threshold_rad_s
    }
    /// Returns the nonzero number of stable checks required.
    #[must_use]
    pub const fn required_stable_checks(self) -> u32 {
        self.required_stable_checks
    }
    /// Returns the finite nonnegative escape guard margin in metres.
    #[must_use]
    pub const fn escape_margin_m(self) -> f32 {
        self.escape_margin_m
    }
    /// Returns the finite nonnegative maximum final penetration in metres.
    #[must_use]
    pub const fn maximum_penetration_m(self) -> f32 {
        self.maximum_penetration_m
    }

    fn try_from_wire(wire: PhysicsWire) -> Result<Self, RunConfigValidationError> {
        let fixed_timestep_s = wire.fixed_timestep_s.unwrap_or(DEFAULT_FIXED_TIMESTEP_S);
        positive_f32(fixed_timestep_s, ConfigScalarField::PhysicsTimestep)?;
        nonzero(wire.substeps, ConfigCountField::PhysicsSubsteps)?;
        nonzero(
            wire.solver_iterations,
            ConfigCountField::PhysicsSolverIterations,
        )?;
        let min_steps = wire.min_steps.unwrap_or(DEFAULT_MIN_STEPS);
        let max_steps = wire.max_steps.unwrap_or(DEFAULT_MAX_STEPS);
        nonzero(min_steps, ConfigCountField::PhysicsMinSteps)?;
        nonzero(max_steps, ConfigCountField::PhysicsMaxSteps)?;
        if min_steps > max_steps {
            return Err(RunConfigValidationError::InvalidPhysicsOrdering {
                min_steps,
                max_steps,
            });
        }
        let stability_check_interval = wire
            .stability_check_interval
            .unwrap_or(DEFAULT_STABILITY_CHECK_INTERVAL);
        let required_stable_checks = wire
            .required_stable_checks
            .unwrap_or(DEFAULT_REQUIRED_STABLE_CHECKS);
        nonzero(
            stability_check_interval,
            ConfigCountField::PhysicsStabilityCheckInterval,
        )?;
        nonzero(
            required_stable_checks,
            ConfigCountField::PhysicsRequiredStableChecks,
        )?;
        if u64::from(stability_check_interval) * u64::from(required_stable_checks)
            > u64::from(max_steps)
        {
            return Err(RunConfigValidationError::StabilityWindowExceedsMaximum {
                interval: stability_check_interval,
                required_checks: required_stable_checks,
                max_steps,
            });
        }
        let linear_sleep_threshold_mps = wire
            .linear_sleep_threshold_mps
            .unwrap_or(DEFAULT_LINEAR_SLEEP_THRESHOLD_MPS);
        let angular_sleep_threshold_rad_s = wire
            .angular_sleep_threshold_rad_s
            .unwrap_or(DEFAULT_ANGULAR_SLEEP_THRESHOLD_RAD_S);
        nonnegative_f32(
            linear_sleep_threshold_mps,
            ConfigScalarField::PhysicsLinearSleepThreshold,
        )?;
        nonnegative_f32(
            angular_sleep_threshold_rad_s,
            ConfigScalarField::PhysicsAngularSleepThreshold,
        )?;
        let escape_margin_m = wire.escape_margin_m.unwrap_or(DEFAULT_ESCAPE_MARGIN_M);
        nonnegative_f32(escape_margin_m, ConfigScalarField::PhysicsEscapeMargin)?;
        let maximum_penetration_m = wire
            .maximum_penetration_m
            .unwrap_or(DEFAULT_MAXIMUM_PENETRATION_M);
        nonnegative_f32(
            maximum_penetration_m,
            ConfigScalarField::PhysicsMaximumPenetration,
        )?;
        Ok(Self {
            fixed_timestep_s,
            substeps: wire.substeps,
            solver_iterations: wire.solver_iterations,
            min_steps,
            max_steps,
            stability_check_interval,
            linear_sleep_threshold_mps: canonicalize_zero(linear_sleep_threshold_mps),
            angular_sleep_threshold_rad_s: canonicalize_zero(angular_sleep_threshold_rad_s),
            required_stable_checks,
            escape_margin_m: canonicalize_zero(escape_margin_m),
            maximum_penetration_m: canonicalize_zero(maximum_penetration_m),
        })
    }
}

/// A checked vertical field of view in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VerticalFovDegrees(f32);

impl VerticalFovDegrees {
    /// Returns the finite value strictly between 0 and 180 degrees.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// Exactly one checked camera projection source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraProjection {
    /// An explicit checked row-major `OpenCV` calibration matrix.
    OpenCvK(CameraIntrinsics),
    /// A vertical field of view strictly between 0 and 180 degrees.
    VerticalFov(VerticalFovDegrees),
}

/// A checked finite shell from which a camera pose may later be sampled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShellCameraPose {
    min_radius_m: PositiveMeters,
    max_radius_m: PositiveMeters,
    min_azimuth_deg: f32,
    max_azimuth_deg: f32,
    min_elevation_deg: f32,
    max_elevation_deg: f32,
    target_bin_m: [Meters; 3],
}

impl ShellCameraPose {
    /// Returns the positive minimum shell radius.
    #[must_use]
    pub const fn min_radius_m(self) -> PositiveMeters {
        self.min_radius_m
    }
    /// Returns the positive maximum shell radius.
    #[must_use]
    pub const fn max_radius_m(self) -> PositiveMeters {
        self.max_radius_m
    }
    /// Returns the finite minimum azimuth in degrees.
    #[must_use]
    pub const fn min_azimuth_deg(self) -> f32 {
        self.min_azimuth_deg
    }
    /// Returns the finite maximum azimuth in degrees.
    #[must_use]
    pub const fn max_azimuth_deg(self) -> f32 {
        self.max_azimuth_deg
    }
    /// Returns the finite minimum elevation in degrees.
    #[must_use]
    pub const fn min_elevation_deg(self) -> f32 {
        self.min_elevation_deg
    }
    /// Returns the finite maximum elevation in degrees.
    #[must_use]
    pub const fn max_elevation_deg(self) -> f32 {
        self.max_elevation_deg
    }
    /// Returns the finite shell target in bin-local metres.
    #[must_use]
    pub const fn target_bin_m(self) -> [Meters; 3] {
        self.target_bin_m
    }
}

/// Exactly one checked camera pose source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraPose {
    /// A fixed transform from a local right-handed Z-up camera frame to world.
    ///
    /// The local frame uses +X right, +Y forward, and +Z up, matching the
    /// input convention accepted by [`crate::CameraExtrinsics::try_from_world_from_z_up_camera`].
    FixedWorldFromCamera(RigidTransform),
    /// A finite shell specification for later deterministic sampling.
    Shell(ShellCameraPose),
}

/// Immutable checked camera configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraConfig {
    id: CameraId,
    image_size: ImageSize,
    near_m: PositiveMeters,
    far_m: PositiveMeters,
    projection: CameraProjection,
    pose: CameraPose,
}

impl CameraConfig {
    /// Returns the stable camera ID.
    #[must_use]
    pub const fn id(self) -> CameraId {
        self.id
    }
    /// Returns the nonzero image size.
    #[must_use]
    pub const fn image_size(self) -> ImageSize {
        self.image_size
    }
    /// Returns the positive near clipping distance.
    #[must_use]
    pub const fn near_m(self) -> PositiveMeters {
        self.near_m
    }
    /// Returns the positive far clipping distance.
    #[must_use]
    pub const fn far_m(self) -> PositiveMeters {
        self.far_m
    }
    /// Returns the sole projection source.
    #[must_use]
    pub const fn projection(self) -> CameraProjection {
        self.projection
    }
    /// Returns the sole pose source.
    #[must_use]
    pub const fn pose(self) -> CameraPose {
        self.pose
    }

    fn try_from_wire(wire: CameraWire) -> Result<Self, RunConfigValidationError> {
        let id = CameraId::new(wire.id);
        let image_size = checked_image_size(wire.image_size, id)?;
        let near_m = positive_meters(wire.near_m, ConfigScalarField::CameraNear)
            .map_err(|_| RunConfigValidationError::InvalidCameraClipping { camera_id: id })?;
        let far_m = positive_meters(wire.far_m, ConfigScalarField::CameraFar)
            .map_err(|_| RunConfigValidationError::InvalidCameraClipping { camera_id: id })?;
        if near_m.get() >= far_m.get() {
            return Err(RunConfigValidationError::InvalidCameraClipping { camera_id: id });
        }
        let projection = checked_projection(wire.projection, id)?;
        let pose = checked_pose(wire.pose, id)?;
        Ok(Self {
            id,
            image_size,
            near_m,
            far_m,
            projection,
            pose,
        })
    }
}

/// Checked appearance snapshot and PBR material defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceConfig {
    snapshot: crate::AppearanceSnapshot,
    material_defaults: PbrMaterialConfig,
}

impl AppearanceConfig {
    /// Returns the checked background, ambient term, and configured-order lights.
    #[must_use]
    pub const fn snapshot(&self) -> &crate::AppearanceSnapshot {
        &self.snapshot
    }
    /// Returns the checked PBR material defaults.
    #[must_use]
    pub const fn material_defaults(&self) -> &PbrMaterialConfig {
        &self.material_defaults
    }

    fn try_from_wire(wire: AppearanceWire) -> Result<Self, RunConfigValidationError> {
        let background_srgb = SrgbColor::try_new(wire.background_srgb)
            .map_err(|_| invalid_appearance(AppearanceField::Background, None))?;
        let ambient_light = checked_ambient(wire.ambient)?;
        let directional_lights = wire
            .directional_lights
            .into_iter()
            .enumerate()
            .map(|(index, light)| checked_directional_light(light, index))
            .collect::<Result<Vec<_>, _>>()?;
        let point_lights = wire
            .point_lights
            .into_iter()
            .enumerate()
            .map(|(index, light)| checked_point_light(light, index))
            .collect::<Result<Vec<_>, _>>()?;
        let material_defaults = PbrMaterialConfig::try_from_wire(wire.material_defaults, None)?;
        Ok(Self {
            snapshot: crate::AppearanceSnapshot::new(
                background_srgb,
                ambient_light,
                directional_lights,
                point_lights,
            ),
            material_defaults,
        })
    }
}

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
    device_preference: WgpuDevicePreference,
    adapter_preference: WgpuAdapterPreference,
    shadow_map_size: u32,
    rgb_fxaa: bool,
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

    fn try_from_wire(wire: WgpuRendererWire) -> Result<Self, RunConfigValidationError> {
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
    executable_path: PathBuf,
    worker_path: PathBuf,
    device_selection: String,
    handshake_timeout_s: f32,
    request_timeout_s: f32,
    stderr_tail_capacity: u32,
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

    fn try_from_wire(wire: BlenderProcRendererWire) -> Result<Self, RunConfigValidationError> {
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
    wgpu: WgpuRendererConfig,
    blenderproc: BlenderProcRendererConfig,
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

    fn try_from_wire(wire: RenderersWire) -> Result<Self, RunConfigValidationError> {
        Ok(Self {
            wgpu: WgpuRendererConfig::try_from_wire(wire.wgpu)?,
            blenderproc: BlenderProcRendererConfig::try_from_wire(wire.blenderproc)?,
        })
    }
}

/// Top-level TOML loading failure.
#[derive(Debug, Error)]
pub enum RunConfigError {
    /// TOML syntax or strict object-shape validation failed.
    #[error("invalid TOML configuration: {0}")]
    Toml(#[from] toml::de::Error),
    /// Parsed values violated a typed semantic invariant.
    #[error("invalid run configuration: {0}")]
    Validation(#[from] RunConfigValidationError),
}

/// Work-count field associated with a zero-value validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigCountField {
    /// Requested valid scene count.
    ValidSceneCount,
    /// Attempts per requested scene.
    SceneAttemptLimit,
    /// Placement attempts per object.
    PlacementAttemptLimitPerObject,
    /// Simulation worker count.
    SimulationWorkers,
    /// Renderer worker count.
    RendererWorkers,
    /// Bounded queue capacity.
    QueueCapacity,
    /// V-HACD voxel resolution.
    VhacdResolution,
    /// V-HACD maximum hull count.
    VhacdMaxConvexHulls,
    /// V-HACD plane downsampling factor.
    VhacdPlaneDownsampling,
    /// V-HACD convex-hull downsampling factor.
    VhacdConvexHullDownsampling,
    /// Physics substeps.
    PhysicsSubsteps,
    /// Physics solver iterations.
    PhysicsSolverIterations,
    /// Physics minimum steps.
    PhysicsMinSteps,
    /// Physics maximum steps.
    PhysicsMaxSteps,
    /// Physics stability-check interval.
    PhysicsStabilityCheckInterval,
    /// Required stable checks.
    PhysicsRequiredStableChecks,
    /// wgpu shadow-map size.
    ShadowMapSize,
    /// `BlenderProc` stderr tail capacity.
    StderrTailCapacity,
}

/// Path field associated with an empty-path validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigPathField {
    /// Dataset output path.
    Output,
    /// Asset cache path.
    Cache,
    /// Asset source path.
    AssetSource,
    /// `BlenderProc` executable path.
    BlenderProcExecutable,
    /// `BlenderProc` worker path.
    BlenderProcWorker,
}

/// Text field associated with a blank-text validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigTextField {
    /// Bin material label.
    BinMaterialLabel,
    /// `BlenderProc` device-selection value.
    BlenderProcDeviceSelection,
}

/// Scalar field associated with a numeric domain failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigScalarField {
    /// Bin inner X dimension.
    BinInnerX,
    /// Bin inner Y dimension.
    BinInnerY,
    /// Bin inner Z dimension.
    BinInnerZ,
    /// Bin floor thickness.
    BinFloorThickness,
    /// Bin wall thickness.
    BinWallThickness,
    /// Bin friction.
    BinFriction,
    /// Bin restitution.
    BinRestitution,
    /// Asset source-unit scale.
    AssetSourceUnitScale,
    /// V-HACD concavity.
    VhacdConcavity,
    /// Physics timestep.
    PhysicsTimestep,
    /// Physics linear sleep threshold.
    PhysicsLinearSleepThreshold,
    /// Physics angular sleep threshold.
    PhysicsAngularSleepThreshold,
    /// Physics escape guard margin.
    PhysicsEscapeMargin,
    /// Physics maximum accepted final penetration.
    PhysicsMaximumPenetration,
    /// Camera near plane.
    CameraNear,
    /// Camera far plane.
    CameraFar,
    /// `BlenderProc` handshake timeout.
    BlenderProcHandshakeTimeout,
    /// `BlenderProc` request timeout.
    BlenderProcRequestTimeout,
}

/// Transform field associated with a malformed rigid transform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigTransformField {
    /// Bin world pose.
    Bin,
    /// Asset model transform.
    AssetModel,
    /// Fixed camera world pose.
    CameraFixedPose,
}

/// Distribution field associated with a checked distribution failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DistributionField {
    /// Rigid-body mass in kilograms.
    MassKg,
    /// Friction coefficient.
    Friction,
    /// Restitution coefficient.
    Restitution,
    /// PBR metallic factor.
    Metallic,
    /// PBR roughness factor.
    Roughness,
    /// Population asset mixture.
    AssetMixture,
    /// Population object count.
    ObjectCount,
}

/// Appearance field associated with invalid checked color or light data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppearanceField {
    /// Scene background color.
    Background,
    /// Ambient color or intensity.
    Ambient,
    /// Directional light entry.
    DirectionalLight,
    /// Point light entry.
    PointLight,
    /// PBR base color.
    BaseColor,
}

/// Shell field associated with invalid bounds or target data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CameraShellField {
    /// Radius bounds.
    Radius,
    /// Azimuth bounds.
    Azimuth,
    /// Elevation bounds.
    Elevation,
    /// Target point.
    Target,
}

/// Typed reason a raw distribution could not become an invariant-preserving value.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum ConfigDistributionError {
    /// The distribution's own variant invariant failed.
    #[error(transparent)]
    Distribution(#[from] InvalidDistributionError),
    /// A choice weight was non-finite or negative.
    #[error("choice entry {index} has an invalid weight: {source}")]
    Weight {
        /// Zero-based choice entry index.
        index: usize,
        /// Checked weight failure.
        source: InvalidWeightedValueError,
    },
    /// A finite distribution value violated the field's physical domain.
    #[error("distribution value {value} violates the field domain")]
    DomainValue {
        /// Rejected value.
        value: f32,
    },
}

/// Typed semantic validation failure for a parsed run configuration.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum RunConfigValidationError {
    /// A work-controlling count or capacity was zero.
    #[error("{field:?} must be nonzero")]
    ZeroCount {
        /// Rejected field.
        field: ConfigCountField,
    },
    /// A configured path was empty or whitespace-only.
    #[error("{field:?} path must be nonempty")]
    InvalidPath {
        /// Rejected path field.
        field: ConfigPathField,
        /// Asset context for asset source paths.
        asset_id: Option<AssetId>,
    },
    /// A configured text field was empty or whitespace-only.
    #[error("{field:?} text must be nonblank")]
    InvalidText {
        /// Rejected text field.
        field: ConfigTextField,
    },
    /// A scalar required to be finite and positive was invalid.
    #[error("{field:?} must be finite and positive, got {value}")]
    InvalidPositiveScalar {
        /// Rejected scalar field.
        field: ConfigScalarField,
        /// Rejected value.
        value: f64,
    },
    /// A scalar required to be finite and nonnegative was invalid.
    #[error("{field:?} must be finite and nonnegative, got {value}")]
    InvalidNonNegativeScalar {
        /// Rejected scalar field.
        field: ConfigScalarField,
        /// Rejected value.
        value: f64,
    },
    /// A scalar required to be finite and in `[0, 1]` was invalid.
    #[error("{field:?} must be finite and in [0, 1], got {value}")]
    InvalidUnitInterval {
        /// Rejected scalar field.
        field: ConfigScalarField,
        /// Rejected value.
        value: f64,
    },
    /// A rigid transform was malformed.
    #[error("{field:?} rigid transform is invalid")]
    InvalidTransform {
        /// Rejected transform field.
        field: ConfigTransformField,
        /// Optional asset context.
        asset_id: Option<AssetId>,
        /// Optional camera context.
        camera_id: Option<CameraId>,
    },
    /// An asset ID was blank.
    #[error("asset ID must be nonblank, got {value:?}")]
    InvalidAssetId {
        /// Rejected raw ID.
        value: String,
    },
    /// An asset catalog was empty.
    #[error("assets must contain at least one entry")]
    EmptyAssets,
    /// Two asset entries used the same ID.
    #[error("duplicate asset ID {asset_id:?}")]
    DuplicateAssetId {
        /// Repeated asset ID.
        asset_id: AssetId,
    },
    /// Every catalog mixture weight was zero.
    #[error("at least one asset mixture weight must be positive")]
    AllZeroAssetWeights,
    /// One asset mixture weight was non-finite or negative.
    #[error("asset {asset_id:?} has invalid mixture weight {value}")]
    InvalidAssetMixtureWeight {
        /// Asset owning the rejected weight.
        asset_id: AssetId,
        /// Rejected weight.
        value: f64,
    },
    /// A typed distribution could not be validated.
    #[error("invalid {field:?} distribution: {source}")]
    InvalidDistribution {
        /// Distribution's semantic role.
        field: DistributionField,
        /// Optional owning asset.
        asset_id: Option<AssetId>,
        /// Typed distribution failure.
        source: ConfigDistributionError,
    },
    /// A population asset-mixture ID was blank.
    #[error("population asset reference must be nonblank, got {value:?}")]
    InvalidAssetReference {
        /// Rejected raw reference.
        value: String,
    },
    /// A population asset mixture referenced no configured asset.
    #[error("unknown asset reference {asset_id:?}")]
    UnknownAssetReference {
        /// Unknown asset ID.
        asset_id: AssetId,
    },
    /// The population distribution admitted zero objects or inverted bounds.
    #[error(
        "population object count must contain only positive ordered values, got {minimum}..={maximum}"
    )]
    InvalidPopulationCount {
        /// Lowest observed or configured count.
        minimum: u32,
        /// Highest observed or configured count.
        maximum: u32,
    },
    /// A spawn interval was non-finite, empty, or inverted.
    #[error("spawn interval on {axis:?} must be finite with min < max")]
    InvalidSpawnAxis {
        /// Rejected axis.
        axis: Axis,
    },
    /// A spawn interval extended beyond the bin interior.
    #[error("spawn interval on {axis:?} extends outside the bin interior")]
    SpawnOutsideBin {
        /// Rejected axis.
        axis: Axis,
    },
    /// Physics minimum steps exceeded maximum steps.
    #[error("physics min_steps {min_steps} exceeds max_steps {max_steps}")]
    InvalidPhysicsOrdering {
        /// Configured minimum.
        min_steps: u32,
        /// Configured maximum.
        max_steps: u32,
    },
    /// Required stability observations cannot fit in the step budget.
    #[error(
        "{required_checks} stability checks every {interval} steps exceed max_steps {max_steps}"
    )]
    StabilityWindowExceedsMaximum {
        /// Check interval in steps.
        interval: u32,
        /// Required observation count.
        required_checks: u32,
        /// Maximum simulation steps.
        max_steps: u32,
    },
    /// No camera was configured.
    #[error("cameras must contain at least one entry")]
    EmptyCameras,
    /// Two cameras used the same stable ID.
    #[error("duplicate camera ID {camera_id:?}")]
    DuplicateCameraId {
        /// Repeated camera ID.
        camera_id: CameraId,
    },
    /// A camera image size contained a zero dimension.
    #[error("camera {camera_id:?} image size must be nonzero")]
    InvalidCameraImageSize {
        /// Owning camera.
        camera_id: CameraId,
    },
    /// A camera clipping plane was invalid or the near plane was not before far.
    #[error("camera {camera_id:?} clipping planes must be positive with near < far")]
    InvalidCameraClipping {
        /// Owning camera.
        camera_id: CameraId,
    },
    /// A camera configured zero or multiple projection sources.
    #[error("camera {camera_id:?} must configure exactly one projection source, got {configured}")]
    InvalidCameraProjectionSources {
        /// Owning camera.
        camera_id: CameraId,
        /// Number of configured sources.
        configured: u8,
    },
    /// A camera configured zero or multiple pose sources.
    #[error("camera {camera_id:?} must configure exactly one pose source, got {configured}")]
    InvalidCameraPoseSources {
        /// Owning camera.
        camera_id: CameraId,
        /// Number of configured sources.
        configured: u8,
    },
    /// An explicit `OpenCV` matrix was invalid.
    #[error("camera {camera_id:?} OpenCV intrinsics are invalid")]
    InvalidCameraIntrinsics {
        /// Owning camera.
        camera_id: CameraId,
    },
    /// A vertical field of view was not finite and strictly between 0 and 180.
    #[error("camera {camera_id:?} vertical FOV must be finite and in (0, 180), got {value}")]
    InvalidVerticalFov {
        /// Owning camera.
        camera_id: CameraId,
        /// Rejected field of view.
        value: f32,
    },
    /// A shell pose field was invalid.
    #[error("camera {camera_id:?} has invalid shell {field:?}")]
    InvalidCameraShell {
        /// Owning camera.
        camera_id: CameraId,
        /// Rejected shell field.
        field: CameraShellField,
    },
    /// Appearance data failed a checked color/light invariant.
    #[error("invalid appearance {field:?} at index {index:?}")]
    InvalidAppearance {
        /// Rejected appearance field.
        field: AppearanceField,
        /// Optional light-list index.
        index: Option<usize>,
    },
    /// Every output channel was disabled.
    #[error("at least one output channel must be requested")]
    NoOutputChannels,
    /// Visible-instance labels cannot represent the maximum population.
    #[error("visible-instance output supports at most 65535 objects, got {maximum}")]
    ExcessiveInstanceCount {
        /// Maximum possible object count.
        maximum: u32,
    },
}

fn validate_assets(wires: Vec<AssetWire>) -> Result<Vec<AssetConfig>, RunConfigValidationError> {
    if wires.is_empty() {
        return Err(RunConfigValidationError::EmptyAssets);
    }
    let mut assets = Vec::with_capacity(wires.len());
    let mut ids = BTreeSet::new();
    for wire in wires {
        let asset = AssetConfig::try_from_wire(wire)?;
        if !ids.insert(asset.id.clone()) {
            return Err(RunConfigValidationError::DuplicateAssetId { asset_id: asset.id });
        }
        assets.push(asset);
    }
    if assets.iter().all(|asset| asset.mixture_weight == 0.0) {
        return Err(RunConfigValidationError::AllZeroAssetWeights);
    }
    assets.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(assets)
}

fn validate_cameras(wires: Vec<CameraWire>) -> Result<Vec<CameraConfig>, RunConfigValidationError> {
    if wires.is_empty() {
        return Err(RunConfigValidationError::EmptyCameras);
    }
    let mut cameras = Vec::with_capacity(wires.len());
    let mut ids = BTreeSet::new();
    for wire in wires {
        let camera = CameraConfig::try_from_wire(wire)?;
        if !ids.insert(camera.id) {
            return Err(RunConfigValidationError::DuplicateCameraId {
                camera_id: camera.id,
            });
        }
        cameras.push(camera);
    }
    cameras.sort_by_key(|camera| camera.id);
    Ok(cameras)
}

fn validate_output(
    wire: OutputWire,
    maximum: u32,
) -> Result<RenderChannels, RunConfigValidationError> {
    let channels = RenderChannels::try_new(wire.rgb, wire.depth, wire.visible_instances)
        .map_err(|_| RunConfigValidationError::NoOutputChannels)?;
    if wire.visible_instances && maximum > u32::from(u16::MAX) {
        return Err(RunConfigValidationError::ExcessiveInstanceCount { maximum });
    }
    Ok(channels)
}

fn checked_object_count(
    wire: DistributionWire<u32>,
) -> Result<Distribution<u32>, RunConfigValidationError> {
    let invalid =
        |minimum, maximum| RunConfigValidationError::InvalidPopulationCount { minimum, maximum };
    match wire {
        DistributionWire::Fixed { value } => {
            if value == 0 {
                return Err(invalid(0, 0));
            }
            Ok(Distribution::<u32>::fixed(value))
        }
        DistributionWire::Uniform { min, max } => {
            if min == 0 || max == 0 || min > max {
                return Err(invalid(min, max));
            }
            Distribution::<u32>::try_uniform(min, max).map_err(|source| {
                invalid_distribution(DistributionField::ObjectCount, None, source.into())
            })
        }
        DistributionWire::Choice { values } => {
            let mut checked = Vec::with_capacity(values.len());
            let mut minimum = u32::MAX;
            let mut maximum = 0;
            for (index, value) in values.into_iter().enumerate() {
                if value.value == 0 {
                    return Err(invalid(0, maximum));
                }
                minimum = minimum.min(value.value);
                maximum = maximum.max(value.value);
                checked.push(WeightedValue::try_new(value.value, value.weight).map_err(
                    |source| {
                        invalid_distribution(
                            DistributionField::ObjectCount,
                            None,
                            ConfigDistributionError::Weight { index, source },
                        )
                    },
                )?);
            }
            Distribution::<u32>::try_choice(checked).map_err(|source| {
                invalid_distribution(DistributionField::ObjectCount, None, source.into())
            })
        }
    }
}

fn checked_asset_distribution(
    wire: DistributionWire<String>,
) -> Result<Distribution<AssetId>, RunConfigValidationError> {
    match wire {
        DistributionWire::Fixed { value } => {
            Ok(Distribution::<AssetId>::fixed(asset_reference(value)?))
        }
        DistributionWire::Uniform { .. } => Err(invalid_distribution(
            DistributionField::AssetMixture,
            None,
            InvalidDistributionError::UniformAsset.into(),
        )),
        DistributionWire::Choice { values } => {
            let mut checked = Vec::with_capacity(values.len());
            for (index, value) in values.into_iter().enumerate() {
                checked.push(
                    WeightedValue::try_new(asset_reference(value.value)?, value.weight).map_err(
                        |source| {
                            invalid_distribution(
                                DistributionField::AssetMixture,
                                None,
                                ConfigDistributionError::Weight { index, source },
                            )
                        },
                    )?,
                );
            }
            Distribution::<AssetId>::try_choice(checked).map_err(|source| {
                invalid_distribution(DistributionField::AssetMixture, None, source.into())
            })
        }
    }
}

fn checked_float_distribution(
    wire: DistributionWire<f32>,
    field: DistributionField,
    asset_id: Option<&AssetId>,
    domain: FloatDomain,
) -> Result<Distribution<f32>, RunConfigValidationError> {
    let distribution = match wire {
        DistributionWire::Fixed { value } => Distribution::<f32>::try_fixed(value)
            .map_err(|source| invalid_distribution(field, asset_id, source.into()))?,
        DistributionWire::Uniform { min, max } => Distribution::<f32>::try_uniform(min, max)
            .map_err(|source| invalid_distribution(field, asset_id, source.into()))?,
        DistributionWire::Choice { values } => {
            let mut checked = Vec::with_capacity(values.len());
            for (index, value) in values.into_iter().enumerate() {
                checked.push(WeightedValue::try_new(value.value, value.weight).map_err(
                    |source| {
                        invalid_distribution(
                            field,
                            asset_id,
                            ConfigDistributionError::Weight { index, source },
                        )
                    },
                )?);
            }
            Distribution::<f32>::try_choice(checked)
                .map_err(|source| invalid_distribution(field, asset_id, source.into()))?
        }
    };
    for value in distribution_values(&distribution) {
        if !domain.accepts(*value) {
            return Err(invalid_distribution(
                field,
                asset_id,
                ConfigDistributionError::DomainValue { value: *value },
            ));
        }
    }
    Ok(distribution)
}

fn distribution_values<T>(distribution: &Distribution<T>) -> Vec<&T> {
    match distribution.view() {
        DistributionView::Fixed { value } => vec![value],
        DistributionView::Uniform { min, max, .. } => vec![min, max],
        DistributionView::Choice { values } => values.iter().map(WeightedValue::value).collect(),
    }
}

fn maximum_count(distribution: &Distribution<u32>) -> u32 {
    match distribution.view() {
        DistributionView::Fixed { value } => *value,
        DistributionView::Uniform { max, .. } => *max,
        DistributionView::Choice { values } => values
            .iter()
            .filter(|entry| entry.weight() > 0.0)
            .map(|entry| *entry.value())
            .max()
            .expect("validated choice distributions have a positive weight"),
    }
}

#[derive(Clone, Copy)]
enum FloatDomain {
    Positive,
    NonNegative,
    UnitInterval,
}

impl FloatDomain {
    fn accepts(self, value: f32) -> bool {
        match self {
            Self::Positive => value > 0.0,
            Self::NonNegative => value >= 0.0,
            Self::UnitInterval => (0.0..=1.0).contains(&value),
        }
    }
}

fn checked_projection(
    wire: CameraProjectionWire,
    camera_id: CameraId,
) -> Result<CameraProjection, RunConfigValidationError> {
    let configured = u8::from(wire.opencv_k.is_some()) + u8::from(wire.vertical_fov_deg.is_some());
    if configured != 1 {
        return Err(RunConfigValidationError::InvalidCameraProjectionSources {
            camera_id,
            configured,
        });
    }
    if let Some(k) = wire.opencv_k {
        return CameraIntrinsics::try_new(k)
            .map(CameraProjection::OpenCvK)
            .map_err(|_| RunConfigValidationError::InvalidCameraIntrinsics { camera_id });
    }
    let value = wire
        .vertical_fov_deg
        .expect("exactly one projection source");
    if !value.is_finite() || !(0.0..180.0).contains(&value) {
        return Err(RunConfigValidationError::InvalidVerticalFov { camera_id, value });
    }
    Ok(CameraProjection::VerticalFov(VerticalFovDegrees(value)))
}

fn checked_pose(
    wire: CameraPoseWire,
    camera_id: CameraId,
) -> Result<CameraPose, RunConfigValidationError> {
    let configured =
        u8::from(wire.fixed_world_from_camera.is_some()) + u8::from(wire.shell.is_some());
    if configured != 1 {
        return Err(RunConfigValidationError::InvalidCameraPoseSources {
            camera_id,
            configured,
        });
    }
    if let Some(transform) = wire.fixed_world_from_camera {
        return checked_transform(
            transform,
            ConfigTransformField::CameraFixedPose,
            None,
            Some(camera_id),
        )
        .map(CameraPose::FixedWorldFromCamera);
    }
    ShellCameraPose::try_from_wire(wire.shell.expect("exactly one pose source"), camera_id)
        .map(CameraPose::Shell)
}

impl ShellCameraPose {
    fn try_from_wire(
        wire: ShellCameraPoseWire,
        camera_id: CameraId,
    ) -> Result<Self, RunConfigValidationError> {
        let min_radius_m = PositiveMeters::try_new(wire.min_radius_m)
            .map_err(|_| invalid_shell(camera_id, CameraShellField::Radius))?;
        let max_radius_m = PositiveMeters::try_new(wire.max_radius_m)
            .map_err(|_| invalid_shell(camera_id, CameraShellField::Radius))?;
        if min_radius_m.get() > max_radius_m.get() {
            return Err(invalid_shell(camera_id, CameraShellField::Radius));
        }
        if !wire.min_azimuth_deg.is_finite()
            || !wire.max_azimuth_deg.is_finite()
            || wire.min_azimuth_deg > wire.max_azimuth_deg
        {
            return Err(invalid_shell(camera_id, CameraShellField::Azimuth));
        }
        if !wire.min_elevation_deg.is_finite()
            || !wire.max_elevation_deg.is_finite()
            || wire.min_elevation_deg > wire.max_elevation_deg
        {
            return Err(invalid_shell(camera_id, CameraShellField::Elevation));
        }
        let target_bin_m = wire.target_bin_m.map(Meters::new);
        let [Some(x), Some(y), Some(z)] = target_bin_m else {
            return Err(invalid_shell(camera_id, CameraShellField::Target));
        };
        Ok(Self {
            min_radius_m,
            max_radius_m,
            min_azimuth_deg: canonicalize_zero(wire.min_azimuth_deg),
            max_azimuth_deg: canonicalize_zero(wire.max_azimuth_deg),
            min_elevation_deg: canonicalize_zero(wire.min_elevation_deg),
            max_elevation_deg: canonicalize_zero(wire.max_elevation_deg),
            target_bin_m: [x, y, z],
        })
    }
}

fn checked_image_size(
    wire: ImageSizeWire,
    camera_id: CameraId,
) -> Result<ImageSize, RunConfigValidationError> {
    ImageSize::try_new(wire.width, wire.height)
        .map_err(|_| RunConfigValidationError::InvalidCameraImageSize { camera_id })
}

fn checked_ambient(wire: AmbientWire) -> Result<AmbientLight, RunConfigValidationError> {
    let color = LinearRgb::try_new(wire.color)
        .map_err(|_| invalid_appearance(AppearanceField::Ambient, None))?;
    let intensity = NonNegativeFinite::try_new(wire.intensity)
        .map_err(|_| invalid_appearance(AppearanceField::Ambient, None))?;
    Ok(AmbientLight::new(color, intensity))
}

fn checked_directional_light(
    wire: DirectionalLightWire,
    index: usize,
) -> Result<DirectionalLight, RunConfigValidationError> {
    let color = LinearRgb::try_new(wire.color)
        .map_err(|_| invalid_appearance(AppearanceField::DirectionalLight, Some(index)))?;
    let illuminance = NonNegativeFinite::try_new(wire.illuminance_lux)
        .map_err(|_| invalid_appearance(AppearanceField::DirectionalLight, Some(index)))?;
    DirectionalLight::try_new(wire.direction_world, color, illuminance)
        .map_err(|_| invalid_appearance(AppearanceField::DirectionalLight, Some(index)))
}

fn checked_point_light(
    wire: PointLightWire,
    index: usize,
) -> Result<PointLight, RunConfigValidationError> {
    let color = LinearRgb::try_new(wire.color)
        .map_err(|_| invalid_appearance(AppearanceField::PointLight, Some(index)))?;
    let power = NonNegativeFinite::try_new(wire.luminous_power_lm)
        .map_err(|_| invalid_appearance(AppearanceField::PointLight, Some(index)))?;
    let position = wire.position_world_m.map(Meters::new);
    let [Some(x), Some(y), Some(z)] = position else {
        return Err(invalid_appearance(AppearanceField::PointLight, Some(index)));
    };
    Ok(PointLight::new([x, y, z], color, power))
}

fn invalid_appearance(field: AppearanceField, index: Option<usize>) -> RunConfigValidationError {
    RunConfigValidationError::InvalidAppearance { field, index }
}

fn invalid_shell(camera_id: CameraId, field: CameraShellField) -> RunConfigValidationError {
    RunConfigValidationError::InvalidCameraShell { camera_id, field }
}

fn invalid_distribution(
    field: DistributionField,
    asset_id: Option<&AssetId>,
    source: ConfigDistributionError,
) -> RunConfigValidationError {
    RunConfigValidationError::InvalidDistribution {
        field,
        asset_id: asset_id.cloned(),
        source,
    }
}

fn asset_reference(value: String) -> Result<AssetId, RunConfigValidationError> {
    AssetId::new(value.clone()).ok_or(RunConfigValidationError::InvalidAssetReference { value })
}

fn checked_transform(
    wire: TransformWire,
    field: ConfigTransformField,
    asset_id: Option<&AssetId>,
    camera_id: Option<CameraId>,
) -> Result<RigidTransform, RunConfigValidationError> {
    RigidTransform::try_new(wire.translation, wire.rotation_xyzw).ok_or_else(|| {
        RunConfigValidationError::InvalidTransform {
            field,
            asset_id: asset_id.cloned(),
            camera_id,
        }
    })
}

fn checked_path(
    value: String,
    field: ConfigPathField,
    asset_id: Option<&AssetId>,
) -> Result<PathBuf, RunConfigValidationError> {
    if value.trim().is_empty() {
        return Err(RunConfigValidationError::InvalidPath {
            field,
            asset_id: asset_id.cloned(),
        });
    }
    Ok(PathBuf::from(value))
}

fn nonzero(value: u32, field: ConfigCountField) -> Result<(), RunConfigValidationError> {
    if value == 0 {
        Err(RunConfigValidationError::ZeroCount { field })
    } else {
        Ok(())
    }
}

fn positive_meters(
    value: f32,
    field: ConfigScalarField,
) -> Result<PositiveMeters, RunConfigValidationError> {
    PositiveMeters::try_new(value).map_err(|_| RunConfigValidationError::InvalidPositiveScalar {
        field,
        value: f64::from(value),
    })
}

fn positive_f32(value: f32, field: ConfigScalarField) -> Result<(), RunConfigValidationError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(RunConfigValidationError::InvalidPositiveScalar {
            field,
            value: f64::from(value),
        })
    }
}

fn nonnegative_f32(value: f32, field: ConfigScalarField) -> Result<(), RunConfigValidationError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(RunConfigValidationError::InvalidNonNegativeScalar {
            field,
            value: f64::from(value),
        })
    }
}

fn unit_interval(value: f32, field: ConfigScalarField) -> Result<(), RunConfigValidationError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(RunConfigValidationError::InvalidUnitInterval {
            field,
            value: f64::from(value),
        })
    }
}

const fn axis(index: usize) -> Axis {
    match index {
        0 => Axis::X,
        1 => Axis::Y,
        _ => Axis::Z,
    }
}

const fn canonicalize_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

const fn canonicalize_zero_f64(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

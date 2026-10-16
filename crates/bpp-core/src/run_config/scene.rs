//! Bin, asset, population, and physics settings.

#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the parent's imports and helpers"
)]
use super::*;

/// Checked static contact properties for the bin interior.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePhysicalProperties {
    pub(super) friction: f32,
    pub(super) restitution: f32,
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
    pub(super) inner_dimensions_m: [PositiveMeters; 3],
    pub(super) floor_thickness_m: PositiveMeters,
    pub(super) wall_thickness_m: PositiveMeters,
    pub(super) world_from_bin: RigidTransform,
    pub(super) material_label: String,
    pub(super) physical_properties: SurfacePhysicalProperties,
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

    pub(super) fn try_from_wire(wire: BinWire) -> Result<Self, RunConfigValidationError> {
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
    pub(super) fn try_from_wire(
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
    pub(super) mass_kg: Distribution<f32>,
    pub(super) friction: Distribution<f32>,
    pub(super) restitution: Distribution<f32>,
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

    pub(super) fn try_from_wire(
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
    pub(super) base_color_srgb: SrgbColor,
    pub(super) metallic: Distribution<f32>,
    pub(super) roughness: Distribution<f32>,
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

    pub(super) fn try_from_wire(
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
    pub(super) resolution: u32,
    pub(super) max_convex_hulls: u32,
    pub(super) concavity: f32,
    pub(super) plane_downsampling: u32,
    pub(super) convex_hull_downsampling: u32,
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

    pub(super) fn try_from_wire(wire: VhacdWire) -> Result<Self, RunConfigValidationError> {
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
    pub(super) id: AssetId,
    pub(super) source_path: PathBuf,
    pub(super) source_unit_scale_m: PositiveMeters,
    pub(super) model_transform: RigidTransform,
    pub(super) mixture_weight: f64,
    pub(super) physical_properties: PhysicalPropertyDistributions,
    pub(super) visual_override: Option<PbrMaterialConfig>,
    pub(super) vhacd: VhacdParameters,
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

    pub(super) fn try_from_wire(wire: AssetWire) -> Result<Self, RunConfigValidationError> {
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
    pub(super) min_m: [f32; 3],
    pub(super) max_m: [f32; 3],
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

    pub(super) fn try_from_wire(
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
    pub(super) object_count: Distribution<u32>,
    pub(super) asset_mixture: Distribution<AssetId>,
    pub(super) spawn_volume: SpawnVolume,
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

    pub(super) fn try_from_wire(
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
    pub(super) fixed_timestep_s: f32,
    pub(super) substeps: u32,
    pub(super) solver_iterations: u32,
    pub(super) min_steps: u32,
    pub(super) max_steps: u32,
    pub(super) stability_check_interval: u32,
    pub(super) linear_sleep_threshold_mps: f32,
    pub(super) angular_sleep_threshold_rad_s: f32,
    pub(super) required_stable_checks: u32,
    pub(super) escape_margin_m: f32,
    pub(super) maximum_penetration_m: f32,
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

    pub(super) fn try_from_wire(wire: PhysicsWire) -> Result<Self, RunConfigValidationError> {
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

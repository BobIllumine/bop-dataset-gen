use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{AssetId, AttemptId, InstanceId, RigidTransform, SceneId, SchemaVersion, SrgbColor};

/// A finite, strictly positive distance measured in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositiveMeters(f32);

impl PositiveMeters {
    /// Creates a positive distance from a finite scalar metre value.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPositiveMetersError`] when `value` is non-finite or
    /// not strictly greater than zero.
    pub fn try_new(value: f32) -> Result<Self, InvalidPositiveMetersError> {
        if value.is_finite() && value > 0.0 {
            Ok(Self(value))
        } else {
            Err(InvalidPositiveMetersError { value })
        }
    }

    /// Returns this distance as a scalar number of metres.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// Error returned when a positive metre value is non-finite or not positive.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
#[error("positive metre value must be finite and greater than zero, got {value}")]
pub struct InvalidPositiveMetersError {
    value: f32,
}

impl InvalidPositiveMetersError {
    /// Returns the rejected scalar metre value.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.value
    }
}

impl TryFrom<f32> for PositiveMeters {
    type Error = InvalidPositiveMetersError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl Serialize for PositiveMeters {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PositiveMeters {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_new(f32::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Backend-neutral physical properties resolved for one rigid body.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ResolvedPhysicalProperties {
    schema_version: SchemaVersion,
    mass_kg: f32,
    friction: f32,
    restitution: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedPhysicalPropertiesWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    mass_kg: f32,
    friction: f32,
    restitution: f32,
}

impl<'de> Deserialize<'de> for ResolvedPhysicalProperties {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ResolvedPhysicalPropertiesWire::deserialize(deserializer)?;
        Self::try_new(wire.mass_kg, wire.friction, wire.restitution).map_err(D::Error::custom)
    }
}

impl ResolvedPhysicalProperties {
    /// Creates resolved properties after validating their physical domains.
    ///
    /// # Errors
    ///
    /// Returns a field-specific error for non-finite or out-of-range values.
    pub fn try_new(
        mass_kg: f32,
        friction: f32,
        restitution: f32,
    ) -> Result<Self, InvalidResolvedPhysicalPropertiesError> {
        if !mass_kg.is_finite() || mass_kg <= 0.0 {
            return Err(InvalidResolvedPhysicalPropertiesError::InvalidMass { value: mass_kg });
        }
        if !friction.is_finite() || friction < 0.0 {
            return Err(InvalidResolvedPhysicalPropertiesError::InvalidFriction {
                value: friction,
            });
        }
        if !restitution.is_finite() || !(0.0..=1.0).contains(&restitution) {
            return Err(InvalidResolvedPhysicalPropertiesError::InvalidRestitution {
                value: restitution,
            });
        }
        Ok(Self {
            schema_version: SchemaVersion::current(),
            mass_kg,
            friction: canonicalize_zero(friction),
            restitution: canonicalize_zero(restitution),
        })
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the rigid-body mass in kilograms.
    #[must_use]
    pub const fn mass_kg(self) -> f32 {
        self.mass_kg
    }

    /// Returns the nonnegative coefficient of friction.
    #[must_use]
    pub const fn friction(self) -> f32 {
        self.friction
    }

    /// Returns the coefficient of restitution in the inclusive range `[0, 1]`.
    #[must_use]
    pub const fn restitution(self) -> f32 {
        self.restitution
    }
}

/// Error returned when resolved physical properties violate their domains.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidResolvedPhysicalPropertiesError {
    /// The mass was non-finite or not strictly positive.
    #[error("mass_kg must be finite and greater than zero, got {value}")]
    InvalidMass {
        /// The rejected mass in kilograms.
        value: f32,
    },
    /// The friction coefficient was non-finite or negative.
    #[error("friction must be finite and nonnegative, got {value}")]
    InvalidFriction {
        /// The rejected friction coefficient.
        value: f32,
    },
    /// The restitution coefficient was non-finite or outside `[0, 1]`.
    #[error("restitution must be finite and in [0, 1], got {value}")]
    InvalidRestitution {
        /// The rejected restitution coefficient.
        value: f32,
    },
}

/// Identifier of the bin a configuration without `[[bins]]` describes, and
/// of bins and instances in snapshots written before bins had identifiers.
pub const DEFAULT_BIN_ID: &str = "bin";

fn default_bin_id() -> String {
    DEFAULT_BIN_ID.to_owned()
}

/// Backend-neutral geometry and pose of one bin.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BinSnapshot {
    schema_version: SchemaVersion,
    id: String,
    inner_dimensions_m: [PositiveMeters; 3],
    floor_thickness_m: PositiveMeters,
    wall_thickness_m: PositiveMeters,
    world_from_bin: RigidTransform,
    material_label: String,
    render_material: BinRenderMaterial,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BinSnapshotWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    #[serde(default = "default_bin_id")]
    id: String,
    inner_dimensions_m: [f32; 3],
    floor_thickness_m: f32,
    wall_thickness_m: f32,
    world_from_bin: RigidTransform,
    material_label: String,
    #[serde(default)]
    render_material: BinRenderMaterial,
}

impl<'de> Deserialize<'de> for BinSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BinSnapshotWire::deserialize(deserializer)?;
        Self::try_new(
            wire.inner_dimensions_m,
            wire.floor_thickness_m,
            wire.wall_thickness_m,
            wire.world_from_bin,
            wire.material_label,
        )
        .and_then(|bin| bin.with_id(wire.id))
        .map(|bin| bin.with_render_material(wire.render_material))
        .map_err(D::Error::custom)
    }
}

impl BinSnapshot {
    /// Creates a bin from positive inner `[x, y, z]` dimensions in metres.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidBinSnapshotError`] with the index of the first invalid
    /// dimension.
    pub fn try_new(
        inner_dimensions_m: [f32; 3],
        floor_thickness_m: f32,
        wall_thickness_m: f32,
        world_from_bin: RigidTransform,
        material_label: impl Into<String>,
    ) -> Result<Self, InvalidBinSnapshotError> {
        let [x, y, z] = inner_dimensions_m;
        let checked_dimension = |index, value| {
            PositiveMeters::try_new(value)
                .map_err(|source| InvalidBinSnapshotError::InvalidInnerDimension { index, source })
        };
        let material_label = material_label.into();
        if material_label.trim().is_empty() {
            return Err(InvalidBinSnapshotError::InvalidMaterialLabel {
                value: material_label,
            });
        }
        Ok(Self {
            schema_version: SchemaVersion::current(),
            id: default_bin_id(),
            inner_dimensions_m: [
                checked_dimension(0, x)?,
                checked_dimension(1, y)?,
                checked_dimension(2, z)?,
            ],
            floor_thickness_m: PositiveMeters::try_new(floor_thickness_m)
                .map_err(InvalidBinSnapshotError::InvalidFloorThickness)?,
            wall_thickness_m: PositiveMeters::try_new(wall_thickness_m)
                .map_err(InvalidBinSnapshotError::InvalidWallThickness)?,
            world_from_bin,
            material_label,
            render_material: BinRenderMaterial::default(),
        })
    }

    /// Replaces the default identifier, [`DEFAULT_BIN_ID`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidBinSnapshotError::InvalidId`] for a blank identifier.
    pub fn with_id(mut self, id: impl Into<String>) -> Result<Self, InvalidBinSnapshotError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(InvalidBinSnapshotError::InvalidId { value: id });
        }
        self.id = id;
        Ok(self)
    }

    /// Returns the bin identifier, unique within its scene.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Replaces the default render material.
    #[must_use]
    pub const fn with_render_material(mut self, render_material: BinRenderMaterial) -> Self {
        self.render_material = render_material;
        self
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

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

    /// Returns the transform from bin coordinates to world coordinates.
    #[must_use]
    pub const fn world_from_bin(&self) -> RigidTransform {
        self.world_from_bin
    }

    /// Returns the nonblank configured material label.
    #[must_use]
    pub fn material_label(&self) -> &str {
        &self.material_label
    }

    /// Returns the surface renderers draw the bin with.
    #[must_use]
    pub const fn render_material(&self) -> BinRenderMaterial {
        self.render_material
    }
}

/// Checked uniform surface of the rendered bin.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BinRenderMaterial {
    base_color_srgb: SrgbColor,
    metallic: f32,
    roughness: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BinRenderMaterialWire {
    base_color_srgb: SrgbColor,
    metallic: f32,
    roughness: f32,
}

impl<'de> Deserialize<'de> for BinRenderMaterial {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BinRenderMaterialWire::deserialize(deserializer)?;
        Self::try_new(wire.base_color_srgb, wire.metallic, wire.roughness).map_err(D::Error::custom)
    }
}

impl Default for BinRenderMaterial {
    /// A light grey, dielectric, moderately rough tote.
    fn default() -> Self {
        Self {
            base_color_srgb: SrgbColor::try_new([0.6, 0.6, 0.62, 1.0])
                .expect("the default bin color is in range"),
            metallic: 0.0,
            roughness: 0.6,
        }
    }
}

impl BinRenderMaterial {
    /// Creates a bin surface from metallic and roughness factors in `[0, 1]`.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidBinRenderMaterialError`] for the first factor that is
    /// non-finite or outside `[0, 1]`.
    pub fn try_new(
        base_color_srgb: SrgbColor,
        metallic: f32,
        roughness: f32,
    ) -> Result<Self, InvalidBinRenderMaterialError> {
        let unit = |value: f32| value.is_finite() && (0.0..=1.0).contains(&value);
        if !unit(metallic) {
            return Err(InvalidBinRenderMaterialError::Metallic { value: metallic });
        }
        if !unit(roughness) {
            return Err(InvalidBinRenderMaterialError::Roughness { value: roughness });
        }
        Ok(Self {
            base_color_srgb,
            metallic: canonicalize_zero(metallic),
            roughness: canonicalize_zero(roughness),
        })
    }

    /// Returns the sRGB base color.
    #[must_use]
    pub const fn base_color_srgb(self) -> SrgbColor {
        self.base_color_srgb
    }

    /// Returns the metallic factor in `[0, 1]`.
    #[must_use]
    pub const fn metallic(self) -> f32 {
        self.metallic
    }

    /// Returns the roughness factor in `[0, 1]`.
    #[must_use]
    pub const fn roughness(self) -> f32 {
        self.roughness
    }
}

/// Error returned when a bin render material factor is out of range.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidBinRenderMaterialError {
    /// The metallic factor was non-finite or outside `[0, 1]`.
    #[error("bin metallic factor must be finite and in [0, 1], got {value}")]
    Metallic {
        /// Rejected factor.
        value: f32,
    },
    /// The roughness factor was non-finite or outside `[0, 1]`.
    #[error("bin roughness factor must be finite and in [0, 1], got {value}")]
    Roughness {
        /// Rejected factor.
        value: f32,
    },
}

/// Error returned when a bin snapshot contains an invalid dimension.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum InvalidBinSnapshotError {
    /// An inner dimension was non-finite or not strictly positive.
    #[error("inner_dimensions_m[{index}] is invalid: {source}")]
    InvalidInnerDimension {
        /// Zero-based `[x, y, z]` dimension index.
        index: usize,
        /// The invalid positive-metre value.
        #[source]
        source: InvalidPositiveMetersError,
    },
    /// The floor thickness was non-finite or not strictly positive.
    #[error("floor_thickness_m is invalid: {0}")]
    InvalidFloorThickness(#[source] InvalidPositiveMetersError),
    /// The wall thickness was non-finite or not strictly positive.
    #[error("wall_thickness_m is invalid: {0}")]
    InvalidWallThickness(#[source] InvalidPositiveMetersError),
    /// The material label was empty or whitespace-only.
    #[error("material_label must contain a non-whitespace character, got {value:?}")]
    InvalidMaterialLabel {
        /// Rejected label.
        value: String,
    },
    /// The identifier was empty or whitespace-only.
    #[error("bin id must contain a non-whitespace character, got {value:?}")]
    InvalidId {
        /// Rejected identifier.
        value: String,
    },
}

/// Backend-neutral identity, pose, and physical properties for one instance.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InstanceSnapshot {
    schema_version: SchemaVersion,
    instance_id: InstanceId,
    asset_id: AssetId,
    bin_id: String,
    world_from_model: RigidTransform,
    physical_properties: ResolvedPhysicalProperties,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceSnapshotWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    instance_id: InstanceId,
    asset_id: AssetId,
    #[serde(default = "default_bin_id")]
    bin_id: String,
    world_from_model: RigidTransform,
    physical_properties: ResolvedPhysicalProperties,
}

impl<'de> Deserialize<'de> for InstanceSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = InstanceSnapshotWire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.instance_id,
            wire.asset_id,
            wire.world_from_model,
            wire.physical_properties,
        )
        .with_bin_id(wire.bin_id))
    }
}

impl InstanceSnapshot {
    /// Creates an instance in the bin [`DEFAULT_BIN_ID`] from previously checked domain values.
    #[must_use]
    pub fn new(
        instance_id: InstanceId,
        asset_id: AssetId,
        world_from_model: RigidTransform,
        physical_properties: ResolvedPhysicalProperties,
    ) -> Self {
        Self {
            schema_version: SchemaVersion::current(),
            instance_id,
            asset_id,
            bin_id: default_bin_id(),
            world_from_model,
            physical_properties,
        }
    }

    /// Records the bin this instance was dropped into.
    #[must_use]
    pub fn with_bin_id(mut self, bin_id: impl Into<String>) -> Self {
        self.bin_id = bin_id.into();
        self
    }

    /// Returns the identifier of the bin this instance was dropped into.
    ///
    /// It names where the instance was spawned, which is also where it settled:
    /// a scene is rejected if any instance leaves its bin.
    #[must_use]
    pub fn bin_id(&self) -> &str {
        &self.bin_id
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the stable foreground instance identifier.
    #[must_use]
    pub const fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    /// Returns the resolved logical asset identifier.
    #[must_use]
    pub const fn asset_id(&self) -> &AssetId {
        &self.asset_id
    }

    /// Returns the transform from model coordinates to world coordinates.
    #[must_use]
    pub const fn world_from_model(&self) -> RigidTransform {
        self.world_from_model
    }

    /// Returns the resolved rigid-body physical properties.
    #[must_use]
    pub const fn physical_properties(&self) -> ResolvedPhysicalProperties {
        self.physical_properties
    }
}

/// Deterministic reason why a physical-scene attempt was rejected.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicsRejectionReason {
    /// The bin could not accept another valid spawn pose.
    SpawnSaturation,
    /// The simulation produced a non-finite body state.
    NonFiniteState,
    /// The simulation exceeded its permitted duration or step budget.
    Timeout,
    /// A body escaped the permitted bin volume.
    Escape,
    /// The final scene contained excessive interpenetration.
    ExcessivePenetration,
    /// The final state failed a physical validity check.
    InvalidFinalState,
}

/// Count of rejected attempts for one deterministic reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RejectionCount {
    schema_version: SchemaVersion,
    reason: PhysicsRejectionReason,
    count: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectionCountWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    reason: PhysicsRejectionReason,
    count: u32,
}

impl<'de> Deserialize<'de> for RejectionCount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RejectionCountWire::deserialize(deserializer)?;
        Ok(Self::new(wire.reason, wire.count))
    }
}

impl RejectionCount {
    /// Creates a rejection count for one reason.
    #[must_use]
    pub const fn new(reason: PhysicsRejectionReason, count: u32) -> Self {
        Self {
            schema_version: SchemaVersion::current(),
            reason,
            count,
        }
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the rejection reason.
    #[must_use]
    pub const fn reason(&self) -> PhysicsRejectionReason {
        self.reason
    }

    /// Returns how many attempts were rejected for this reason.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }
}

/// Backend-neutral diagnostic summary of accepted physics generation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PhysicsReport {
    schema_version: SchemaVersion,
    attempts: u32,
    steps: u64,
    simulated_seconds: f32,
    final_max_linear_speed_mps: f32,
    final_max_angular_speed_rad_s: f32,
    final_max_penetration_m: f32,
    rejection_counts: Vec<RejectionCount>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PhysicsReportWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    attempts: u32,
    steps: u64,
    simulated_seconds: f32,
    final_max_linear_speed_mps: f32,
    final_max_angular_speed_rad_s: f32,
    final_max_penetration_m: f32,
    rejection_counts: Vec<RejectionCount>,
}

impl<'de> Deserialize<'de> for PhysicsReport {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PhysicsReportWire::deserialize(deserializer)?;
        Self::try_new(
            wire.attempts,
            wire.steps,
            wire.simulated_seconds,
            wire.final_max_linear_speed_mps,
            wire.final_max_angular_speed_rad_s,
            wire.final_max_penetration_m,
            wire.rejection_counts,
        )
        .map_err(D::Error::custom)
    }
}

impl PhysicsReport {
    /// Creates a report after validating numeric fields and rejection counts.
    ///
    /// Rejection counts may arrive in any order and are stored by declaration
    /// order of [`PhysicsRejectionReason`].
    ///
    /// # Errors
    ///
    /// Returns a field-specific error for invalid numerics or a duplicate
    /// rejection reason.
    pub fn try_new(
        attempts: u32,
        steps: u64,
        simulated_seconds: f32,
        final_max_linear_speed_mps: f32,
        final_max_angular_speed_rad_s: f32,
        final_max_penetration_m: f32,
        mut rejection_counts: Vec<RejectionCount>,
    ) -> Result<Self, InvalidPhysicsReportError> {
        if attempts == 0 {
            return Err(InvalidPhysicsReportError::ZeroAttempts);
        }
        validate_nonnegative_finite(simulated_seconds).map_err(|()| {
            InvalidPhysicsReportError::InvalidSimulatedSeconds {
                value: simulated_seconds,
            }
        })?;
        validate_nonnegative_finite(final_max_linear_speed_mps).map_err(|()| {
            InvalidPhysicsReportError::InvalidFinalMaxLinearSpeed {
                value: final_max_linear_speed_mps,
            }
        })?;
        validate_nonnegative_finite(final_max_angular_speed_rad_s).map_err(|()| {
            InvalidPhysicsReportError::InvalidFinalMaxAngularSpeed {
                value: final_max_angular_speed_rad_s,
            }
        })?;
        validate_nonnegative_finite(final_max_penetration_m).map_err(|()| {
            InvalidPhysicsReportError::InvalidFinalMaxPenetration {
                value: final_max_penetration_m,
            }
        })?;

        rejection_counts.sort_by_key(RejectionCount::reason);
        if let Some(duplicate) = rejection_counts
            .windows(2)
            .find(|pair| pair[0].reason() == pair[1].reason())
        {
            return Err(InvalidPhysicsReportError::DuplicateRejectionReason {
                reason: duplicate[0].reason(),
            });
        }

        Ok(Self {
            schema_version: SchemaVersion::current(),
            attempts,
            steps,
            simulated_seconds: canonicalize_zero(simulated_seconds),
            final_max_linear_speed_mps: canonicalize_zero(final_max_linear_speed_mps),
            final_max_angular_speed_rad_s: canonicalize_zero(final_max_angular_speed_rad_s),
            final_max_penetration_m: canonicalize_zero(final_max_penetration_m),
            rejection_counts,
        })
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the number of attempted scene generations, including acceptance.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Returns the number of simulation steps in the accepted attempt.
    #[must_use]
    pub const fn steps(&self) -> u64 {
        self.steps
    }

    /// Returns the simulated duration in seconds.
    #[must_use]
    pub const fn simulated_seconds(&self) -> f32 {
        self.simulated_seconds
    }

    /// Returns the final maximum linear speed in metres per second.
    #[must_use]
    pub const fn final_max_linear_speed_mps(&self) -> f32 {
        self.final_max_linear_speed_mps
    }

    /// Returns the final maximum angular speed in radians per second.
    #[must_use]
    pub const fn final_max_angular_speed_rad_s(&self) -> f32 {
        self.final_max_angular_speed_rad_s
    }

    /// Returns the final maximum contact penetration in metres.
    #[must_use]
    pub const fn final_max_penetration_m(&self) -> f32 {
        self.final_max_penetration_m
    }

    /// Returns rejection counts in deterministic rejection-reason order.
    #[must_use]
    pub fn rejection_counts(&self) -> &[RejectionCount] {
        &self.rejection_counts
    }
}

/// Error returned when a physics report violates its numeric or uniqueness invariants.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidPhysicsReportError {
    /// The attempt count was zero.
    #[error("attempts must be at least one")]
    ZeroAttempts,
    /// The simulated duration was non-finite or negative.
    #[error("simulated_seconds must be finite and nonnegative, got {value}")]
    InvalidSimulatedSeconds {
        /// The rejected duration in seconds.
        value: f32,
    },
    /// The final maximum linear speed was non-finite or negative.
    #[error("final_max_linear_speed_mps must be finite and nonnegative, got {value}")]
    InvalidFinalMaxLinearSpeed {
        /// The rejected speed in metres per second.
        value: f32,
    },
    /// The final maximum angular speed was non-finite or negative.
    #[error("final_max_angular_speed_rad_s must be finite and nonnegative, got {value}")]
    InvalidFinalMaxAngularSpeed {
        /// The rejected speed in radians per second.
        value: f32,
    },
    /// The final maximum penetration was non-finite or negative.
    #[error("final_max_penetration_m must be finite and nonnegative, got {value}")]
    InvalidFinalMaxPenetration {
        /// The rejected penetration in metres.
        value: f32,
    },
    /// A rejection reason appeared more than once.
    #[error("duplicate rejection reason {reason:?}")]
    DuplicateRejectionReason {
        /// The duplicated reason.
        reason: PhysicsRejectionReason,
    },
}

/// Immutable backend-neutral physical scene consumed by renderers.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SceneSnapshot {
    schema_version: SchemaVersion,
    scene_id: SceneId,
    accepted_attempt: AttemptId,
    seed_fingerprint: String,
    bins: Vec<BinSnapshot>,
    instances: Vec<InstanceSnapshot>,
    physics: PhysicsReport,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneSnapshotWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    scene_id: SceneId,
    accepted_attempt: AttemptId,
    seed_fingerprint: String,
    /// Written before scenes could hold several bins.
    #[serde(default)]
    bin: Option<BinSnapshot>,
    #[serde(default)]
    bins: Option<Vec<BinSnapshot>>,
    instances: Vec<InstanceSnapshot>,
    physics: PhysicsReport,
}

impl<'de> Deserialize<'de> for SceneSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SceneSnapshotWire::deserialize(deserializer)?;
        if let Some(unsorted) = wire
            .instances
            .windows(2)
            .find(|pair| pair[0].instance_id() > pair[1].instance_id())
        {
            return Err(D::Error::custom(
                InvalidSceneSnapshotError::UnsortedInstanceIds {
                    previous_instance_id: unsorted[0].instance_id(),
                    instance_id: unsorted[1].instance_id(),
                },
            ));
        }
        let bins = match (wire.bin, wire.bins) {
            (Some(bin), None) => vec![bin],
            (None, Some(bins)) => bins,
            _ => {
                return Err(D::Error::custom(
                    "a scene has exactly one of `bin` and `bins`",
                ));
            }
        };
        Self::try_new(
            wire.scene_id,
            wire.accepted_attempt,
            wire.seed_fingerprint,
            bins,
            wire.instances,
            wire.physics,
        )
        .map_err(D::Error::custom)
    }
}

impl SceneSnapshot {
    /// Creates a scene, sorting instances by ascending [`InstanceId`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidSceneSnapshotError`] when the seed fingerprint is
    /// empty, there is no bin, a bin or instance identifier is duplicated, or
    /// an instance names a bin the scene does not have.
    pub fn try_new(
        scene_id: SceneId,
        accepted_attempt: AttemptId,
        seed_fingerprint: impl Into<String>,
        bins: Vec<BinSnapshot>,
        mut instances: Vec<InstanceSnapshot>,
        physics: PhysicsReport,
    ) -> Result<Self, InvalidSceneSnapshotError> {
        let seed_fingerprint = seed_fingerprint.into();
        if seed_fingerprint.is_empty() {
            return Err(InvalidSceneSnapshotError::EmptySeedFingerprint);
        }
        if bins.is_empty() {
            return Err(InvalidSceneSnapshotError::NoBins);
        }
        for (index, bin) in bins.iter().enumerate() {
            if bins[..index].iter().any(|earlier| earlier.id() == bin.id()) {
                return Err(InvalidSceneSnapshotError::DuplicateBinId {
                    bin_id: bin.id().to_owned(),
                });
            }
        }
        if let Some(instance) = instances
            .iter()
            .find(|instance| !bins.iter().any(|bin| bin.id() == instance.bin_id()))
        {
            return Err(InvalidSceneSnapshotError::UnknownBinId {
                instance_id: instance.instance_id(),
                bin_id: instance.bin_id().to_owned(),
            });
        }

        instances.sort_by_key(InstanceSnapshot::instance_id);
        if let Some(duplicate) = instances
            .windows(2)
            .find(|pair| pair[0].instance_id() == pair[1].instance_id())
        {
            return Err(InvalidSceneSnapshotError::DuplicateInstanceId {
                instance_id: duplicate[0].instance_id(),
            });
        }

        Ok(Self {
            schema_version: SchemaVersion::current(),
            scene_id,
            accepted_attempt,
            seed_fingerprint,
            bins,
            instances,
            physics,
        })
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the stable scene identifier.
    #[must_use]
    pub const fn scene_id(&self) -> SceneId {
        self.scene_id
    }

    /// Returns the zero-based attempt accepted for this scene.
    #[must_use]
    pub const fn accepted_attempt(&self) -> AttemptId {
        self.accepted_attempt
    }

    /// Returns the nonempty deterministic seed fingerprint.
    #[must_use]
    pub fn seed_fingerprint(&self) -> &str {
        &self.seed_fingerprint
    }

    /// Returns the scene's bins in configuration order; never empty.
    #[must_use]
    pub fn bins(&self) -> &[BinSnapshot] {
        &self.bins
    }

    /// Returns instances sorted by ascending [`InstanceId`].
    #[must_use]
    pub fn instances(&self) -> &[InstanceSnapshot] {
        &self.instances
    }

    /// Returns the accepted physics-generation report.
    #[must_use]
    pub const fn physics(&self) -> &PhysicsReport {
        &self.physics
    }
}

/// Error returned when a scene violates seed or instance identity invariants.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum InvalidSceneSnapshotError {
    /// The seed fingerprint was empty.
    #[error("seed_fingerprint must not be empty")]
    EmptySeedFingerprint,
    /// The scene had no bin.
    #[error("a scene needs at least one bin")]
    NoBins,
    /// Two bins shared an identifier.
    #[error("duplicate bin ID {bin_id:?}")]
    DuplicateBinId {
        /// The duplicated bin identifier.
        bin_id: String,
    },
    /// An instance named a bin the scene does not have.
    #[error("instance {instance_id:?} names unknown bin {bin_id:?}")]
    UnknownBinId {
        /// The instance.
        instance_id: InstanceId,
        /// The unknown bin identifier.
        bin_id: String,
    },
    /// An instance identifier appeared more than once.
    #[error("duplicate instance ID {instance_id:?}")]
    DuplicateInstanceId {
        /// The duplicated instance identifier.
        instance_id: InstanceId,
    },
    /// Instance IDs on the wire were not sorted in ascending order.
    #[error(
        "wire instance IDs must be sorted ascending, got {instance_id:?} after {previous_instance_id:?}"
    )]
    UnsortedInstanceIds {
        /// The preceding, larger instance identifier.
        previous_instance_id: InstanceId,
        /// The following, smaller instance identifier.
        instance_id: InstanceId,
    },
}

fn validate_nonnegative_finite(value: f32) -> Result<(), ()> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(())
    }
}

const fn canonicalize_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

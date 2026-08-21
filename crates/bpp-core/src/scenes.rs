use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{AssetId, AttemptId, InstanceId, RigidTransform, SceneId, SchemaVersion};

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

/// Backend-neutral geometry and pose of the containing bin.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BinSnapshot {
    schema_version: SchemaVersion,
    inner_dimensions_m: [PositiveMeters; 3],
    world_from_bin: RigidTransform,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BinSnapshotWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    inner_dimensions_m: [f32; 3],
    world_from_bin: RigidTransform,
}

impl<'de> Deserialize<'de> for BinSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BinSnapshotWire::deserialize(deserializer)?;
        Self::try_new(wire.inner_dimensions_m, wire.world_from_bin).map_err(D::Error::custom)
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
        world_from_bin: RigidTransform,
    ) -> Result<Self, InvalidBinSnapshotError> {
        let [x, y, z] = inner_dimensions_m;
        let checked_dimension = |index, value| {
            PositiveMeters::try_new(value)
                .map_err(|source| InvalidBinSnapshotError::InvalidInnerDimension { index, source })
        };
        Ok(Self {
            schema_version: SchemaVersion::current(),
            inner_dimensions_m: [
                checked_dimension(0, x)?,
                checked_dimension(1, y)?,
                checked_dimension(2, z)?,
            ],
            world_from_bin,
        })
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns positive inner `[x, y, z]` dimensions in metres.
    #[must_use]
    pub const fn inner_dimensions_m(self) -> [PositiveMeters; 3] {
        self.inner_dimensions_m
    }

    /// Returns the transform from bin coordinates to world coordinates.
    #[must_use]
    pub const fn world_from_bin(self) -> RigidTransform {
        self.world_from_bin
    }
}

/// Error returned when a bin snapshot contains an invalid dimension.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
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
}

/// Backend-neutral identity, pose, and physical properties for one instance.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InstanceSnapshot {
    schema_version: SchemaVersion,
    instance_id: InstanceId,
    asset_id: AssetId,
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
        ))
    }
}

impl InstanceSnapshot {
    /// Creates an instance from previously checked domain values.
    #[must_use]
    pub const fn new(
        instance_id: InstanceId,
        asset_id: AssetId,
        world_from_model: RigidTransform,
        physical_properties: ResolvedPhysicalProperties,
    ) -> Self {
        Self {
            schema_version: SchemaVersion::current(),
            instance_id,
            asset_id,
            world_from_model,
            physical_properties,
        }
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
    bin: BinSnapshot,
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
    bin: BinSnapshot,
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
        Self::try_new(
            wire.scene_id,
            wire.accepted_attempt,
            wire.seed_fingerprint,
            wire.bin,
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
    /// empty or an instance identifier is duplicated.
    pub fn try_new(
        scene_id: SceneId,
        accepted_attempt: AttemptId,
        seed_fingerprint: impl Into<String>,
        bin: BinSnapshot,
        mut instances: Vec<InstanceSnapshot>,
        physics: PhysicsReport,
    ) -> Result<Self, InvalidSceneSnapshotError> {
        let seed_fingerprint = seed_fingerprint.into();
        if seed_fingerprint.is_empty() {
            return Err(InvalidSceneSnapshotError::EmptySeedFingerprint);
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
            bin,
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

    /// Returns the containing bin snapshot.
    #[must_use]
    pub const fn bin(&self) -> &BinSnapshot {
        &self.bin
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
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidSceneSnapshotError {
    /// The seed fingerprint was empty.
    #[error("seed_fingerprint must not be empty")]
    EmptySeedFingerprint,
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

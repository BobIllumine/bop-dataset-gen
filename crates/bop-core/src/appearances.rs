use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use glam::Vec3;

use crate::{Meters, SchemaVersion};

/// A checked transparent sRGB color with alpha in the inclusive range `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SrgbColor([f32; 4]);

impl SrgbColor {
    /// Creates a color from `[red, green, blue, alpha]` components.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidSrgbColorError`] for the first non-finite component or
    /// component outside the inclusive range `[0, 1]`.
    pub fn try_new(rgba: [f32; 4]) -> Result<Self, InvalidSrgbColorError> {
        for (index, value) in rgba.into_iter().enumerate() {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(InvalidSrgbColorError { index, value });
            }
        }
        Ok(Self(rgba.map(canonicalize_zero)))
    }

    /// Returns the checked `[red, green, blue, alpha]` components.
    #[must_use]
    pub const fn rgba(self) -> [f32; 4] {
        self.0
    }
}

impl Serialize for SrgbColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SrgbColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_new(<[f32; 4]>::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Error returned when an sRGB color component violates its checked range.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
#[error("sRGB component rgba[{index}] must be finite and in [0, 1], got {value}")]
pub struct InvalidSrgbColorError {
    index: usize,
    value: f32,
}

impl InvalidSrgbColorError {
    /// Returns the zero-based index of the rejected component.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }

    /// Returns the rejected component value.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.value
    }
}

/// A checked transparent linear RGB color with unbounded nonnegative components.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearRgb([f32; 3]);

impl LinearRgb {
    /// Creates a linear RGB color from finite nonnegative components.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidLinearRgbError`] for the first non-finite or negative
    /// component.
    pub fn try_new(rgb: [f32; 3]) -> Result<Self, InvalidLinearRgbError> {
        for (index, value) in rgb.into_iter().enumerate() {
            if !value.is_finite() || value < 0.0 {
                return Err(InvalidLinearRgbError { index, value });
            }
        }
        Ok(Self(rgb.map(canonicalize_zero)))
    }

    /// Returns the checked linear `[red, green, blue]` components.
    #[must_use]
    pub const fn rgb(self) -> [f32; 3] {
        self.0
    }
}

impl Serialize for LinearRgb {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LinearRgb {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_new(<[f32; 3]>::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Error returned when a linear RGB component is non-finite or negative.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
#[error("linear RGB component rgb[{index}] must be finite and nonnegative, got {value}")]
pub struct InvalidLinearRgbError {
    index: usize,
    value: f32,
}

impl InvalidLinearRgbError {
    /// Returns the zero-based index of the rejected component.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }

    /// Returns the rejected component value.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.value
    }
}

/// A checked transparent scalar that is finite and nonnegative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NonNegativeFinite(f32);

impl NonNegativeFinite {
    /// Creates a checked finite, nonnegative scalar.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidNonNegativeFiniteError`] when `value` is non-finite or
    /// negative.
    pub fn try_new(value: f32) -> Result<Self, InvalidNonNegativeFiniteError> {
        if value.is_finite() && value >= 0.0 {
            Ok(Self(canonicalize_zero(value)))
        } else {
            Err(InvalidNonNegativeFiniteError { value })
        }
    }

    /// Returns the checked scalar value.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for NonNegativeFinite {
    type Error = InvalidNonNegativeFiniteError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl Serialize for NonNegativeFinite {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NonNegativeFinite {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_new(f32::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Error returned when a scalar is non-finite or negative.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
#[error("value must be finite and nonnegative, got {value}")]
pub struct InvalidNonNegativeFiniteError {
    value: f32,
}

impl InvalidNonNegativeFiniteError {
    /// Returns the rejected scalar value.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.value
    }
}

/// Versioned ambient illumination applied uniformly to a scene.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct AmbientLight {
    schema_version: SchemaVersion,
    color: LinearRgb,
    intensity: NonNegativeFinite,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AmbientLightWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    color: LinearRgb,
    intensity: NonNegativeFinite,
}

impl<'de> Deserialize<'de> for AmbientLight {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AmbientLightWire::deserialize(deserializer)?;
        Ok(Self::new(wire.color, wire.intensity))
    }
}

impl AmbientLight {
    /// Creates ambient illumination from checked color and intensity values.
    #[must_use]
    pub const fn new(color: LinearRgb, intensity: NonNegativeFinite) -> Self {
        Self {
            schema_version: SchemaVersion::current(),
            color,
            intensity,
        }
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the linear RGB illumination color.
    #[must_use]
    pub const fn color(self) -> LinearRgb {
        self.color
    }

    /// Returns the dimensionless nonnegative intensity multiplier.
    #[must_use]
    pub const fn intensity(self) -> NonNegativeFinite {
        self.intensity
    }
}

/// Versioned directional illumination in canonical world coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct DirectionalLight {
    schema_version: SchemaVersion,
    direction_world: [f32; 3],
    color: LinearRgb,
    illuminance_lux: NonNegativeFinite,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectionalLightWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    direction_world: [f32; 3],
    color: LinearRgb,
    illuminance_lux: NonNegativeFinite,
}

impl<'de> Deserialize<'de> for DirectionalLight {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = DirectionalLightWire::deserialize(deserializer)?;
        Self::try_new(wire.direction_world, wire.color, wire.illuminance_lux)
            .map_err(D::Error::custom)
    }
}

impl DirectionalLight {
    /// Creates a light and stores its normalized world-space direction.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidDirectionalLightError`] when a direction component is
    /// non-finite or the complete direction is zero.
    pub fn try_new(
        direction_world: [f32; 3],
        color: LinearRgb,
        illuminance_lux: NonNegativeFinite,
    ) -> Result<Self, InvalidDirectionalLightError> {
        for (index, value) in direction_world.into_iter().enumerate() {
            if !value.is_finite() {
                return Err(InvalidDirectionalLightError::NonFiniteDirection { index, value });
            }
        }

        let direction = Vec3::from_array(direction_world);
        let largest_component = direction.abs().max_element();
        if largest_component == 0.0 {
            return Err(InvalidDirectionalLightError::ZeroDirection);
        }
        let normalized = (direction / largest_component).normalize().to_array();

        Ok(Self {
            schema_version: SchemaVersion::current(),
            direction_world: normalized.map(canonicalize_zero),
            color,
            illuminance_lux,
        })
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the normalized direction in canonical world coordinates.
    #[must_use]
    pub const fn direction_world(self) -> [f32; 3] {
        self.direction_world
    }

    /// Returns the linear RGB illumination color.
    #[must_use]
    pub const fn color(self) -> LinearRgb {
        self.color
    }

    /// Returns the nonnegative illuminance in lux.
    #[must_use]
    pub const fn illuminance_lux(self) -> NonNegativeFinite {
        self.illuminance_lux
    }
}

/// Error returned when a directional light has an invalid world direction.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidDirectionalLightError {
    /// A world-direction component was non-finite.
    #[error("direction_world[{index}] must be finite, got {value}")]
    NonFiniteDirection {
        /// Zero-based index of the rejected direction component.
        index: usize,
        /// Rejected direction component.
        value: f32,
    },
    /// The world direction had zero length.
    #[error("direction_world must be nonzero")]
    ZeroDirection,
}

/// Versioned point illumination positioned in canonical world coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PointLight {
    schema_version: SchemaVersion,
    position_world_m: [Meters; 3],
    color: LinearRgb,
    luminous_power_lm: NonNegativeFinite,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PointLightWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    position_world_m: [Meters; 3],
    color: LinearRgb,
    luminous_power_lm: NonNegativeFinite,
}

impl<'de> Deserialize<'de> for PointLight {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PointLightWire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.position_world_m,
            wire.color,
            wire.luminous_power_lm,
        ))
    }
}

impl PointLight {
    /// Creates a point light from checked world position, color, and power values.
    #[must_use]
    pub const fn new(
        position_world_m: [Meters; 3],
        color: LinearRgb,
        luminous_power_lm: NonNegativeFinite,
    ) -> Self {
        Self {
            schema_version: SchemaVersion::current(),
            position_world_m,
            color,
            luminous_power_lm,
        }
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the position in canonical world coordinates, in metres.
    #[must_use]
    pub const fn position_world_m(self) -> [Meters; 3] {
        self.position_world_m
    }

    /// Returns the linear RGB illumination color.
    #[must_use]
    pub const fn color(self) -> LinearRgb {
        self.color
    }

    /// Returns the nonnegative luminous power in lumens.
    #[must_use]
    pub const fn luminous_power_lm(self) -> NonNegativeFinite {
        self.luminous_power_lm
    }
}

/// Image-based lighting from the run's environment map, which replaces the
/// uniform ambient light. The image itself travels with the asset catalog.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct EnvironmentLight {
    intensity: NonNegativeFinite,
    rotation_deg: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentLightWire {
    intensity: NonNegativeFinite,
    rotation_deg: f32,
}

impl<'de> Deserialize<'de> for EnvironmentLight {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EnvironmentLightWire::deserialize(deserializer)?;
        Self::try_new(wire.intensity, wire.rotation_deg).map_err(D::Error::custom)
    }
}

impl EnvironmentLight {
    /// Creates environment lighting scaled by `intensity` and turned by
    /// `rotation_deg` about the world +Z axis.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvironmentRotationError`] for a non-finite rotation.
    pub fn try_new(
        intensity: NonNegativeFinite,
        rotation_deg: f32,
    ) -> Result<Self, InvalidEnvironmentRotationError> {
        if !rotation_deg.is_finite() {
            return Err(InvalidEnvironmentRotationError);
        }
        Ok(Self {
            intensity,
            rotation_deg: canonicalize_zero(rotation_deg),
        })
    }

    /// Returns the radiance scale.
    #[must_use]
    pub const fn intensity(self) -> NonNegativeFinite {
        self.intensity
    }

    /// Returns the rotation about world +Z in degrees.
    #[must_use]
    pub const fn rotation_deg(self) -> f32 {
        self.rotation_deg
    }
}

/// Error returned when an environment rotation is not finite.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("environment rotation must be finite")]
pub struct InvalidEnvironmentRotationError;

/// Immutable versioned appearance inputs for rendering one scene.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AppearanceSnapshot {
    schema_version: SchemaVersion,
    background_srgb: SrgbColor,
    ambient_light: AmbientLight,
    directional_lights: Vec<DirectionalLight>,
    point_lights: Vec<PointLight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<EnvironmentLight>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppearanceSnapshotWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    background_srgb: SrgbColor,
    ambient_light: AmbientLight,
    directional_lights: Vec<DirectionalLight>,
    point_lights: Vec<PointLight>,
    #[serde(default)]
    environment: Option<EnvironmentLight>,
}

impl<'de> Deserialize<'de> for AppearanceSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AppearanceSnapshotWire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.background_srgb,
            wire.ambient_light,
            wire.directional_lights,
            wire.point_lights,
        )
        .with_environment(wire.environment))
    }
}

impl AppearanceSnapshot {
    /// Creates an appearance snapshot while preserving configured light order.
    #[must_use]
    pub fn new(
        background_srgb: SrgbColor,
        ambient_light: AmbientLight,
        directional_lights: Vec<DirectionalLight>,
        point_lights: Vec<PointLight>,
    ) -> Self {
        Self {
            schema_version: SchemaVersion::current(),
            background_srgb,
            ambient_light,
            directional_lights,
            point_lights,
            environment: None,
        }
    }

    /// Lights the scene with the catalog's environment map instead of the
    /// uniform ambient light.
    #[must_use]
    pub const fn with_environment(mut self, environment: Option<EnvironmentLight>) -> Self {
        self.environment = environment;
        self
    }

    /// Returns the environment lighting, when an environment map replaces the ambient light.
    #[must_use]
    pub const fn environment(&self) -> Option<EnvironmentLight> {
        self.environment
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the transparent sRGB background color.
    #[must_use]
    pub const fn background_srgb(&self) -> SrgbColor {
        self.background_srgb
    }

    /// Returns the ambient illumination.
    #[must_use]
    pub const fn ambient_light(&self) -> AmbientLight {
        self.ambient_light
    }

    /// Returns directional lights in configured sampling order.
    #[must_use]
    pub fn directional_lights(&self) -> &[DirectionalLight] {
        &self.directional_lights
    }

    /// Returns point lights in configured sampling order.
    #[must_use]
    pub fn point_lights(&self) -> &[PointLight] {
        &self.point_lights
    }
}

const fn canonicalize_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

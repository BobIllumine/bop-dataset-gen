use std::{fmt, num::NonZeroU16};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

/// Stable identifier assigned to a requested scene.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SceneId(u64);

impl SceneId {
    /// Creates a scene identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric scene identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Zero-based attempt number for generating one scene.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttemptId(u32);

impl AttemptId {
    /// Creates an attempt identifier.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric attempt identifier.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Stable identifier assigned to a camera within a scene.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CameraId(u16);

impl CameraId {
    /// Creates a camera identifier.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the numeric camera identifier.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Stable logical identifier for an asset in the resolved catalog.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AssetId(String);

impl AssetId {
    /// Creates an asset identifier when `value` contains a non-whitespace
    /// character.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    /// Returns the configured asset identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable foreground-object identifier used in rendered instance channels.
///
/// Zero is not representable because dataset encodings reserve it for the
/// background.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InstanceId(NonZeroU16);

impl InstanceId {
    /// Creates an identifier, returning `None` for the reserved background ID.
    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the encoded foreground identifier.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// Error returned when an asset identifier is empty or whitespace-only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidAssetIdError;

impl fmt::Display for InvalidAssetIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("asset ID must contain a non-whitespace character")
    }
}

impl std::error::Error for InvalidAssetIdError {}

/// Error returned when an instance identifier is the reserved background ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidInstanceIdError;

impl fmt::Display for InvalidInstanceIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("instance ID zero is reserved for the background")
    }
}

impl std::error::Error for InvalidInstanceIdError {}

impl TryFrom<String> for AssetId {
    type Error = InvalidAssetIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(InvalidAssetIdError)
    }
}

impl TryFrom<u16> for InstanceId {
    type Error = InvalidInstanceIdError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(InvalidInstanceIdError)
    }
}

macro_rules! transparent_unsigned_identifier {
    ($identifier:ident, $value:ty) => {
        impl Serialize for $identifier {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                self.0.serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for $identifier {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Ok(Self::new(<$value>::deserialize(deserializer)?))
            }
        }
    };
}

transparent_unsigned_identifier!(SceneId, u64);
transparent_unsigned_identifier!(AttemptId, u32);
transparent_unsigned_identifier!(CameraId, u16);

impl Serialize for AssetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .try_into()
            .map_err(D::Error::custom)
    }
}

impl Serialize for InstanceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.get().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for InstanceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        u16::deserialize(deserializer)?
            .try_into()
            .map_err(D::Error::custom)
    }
}

/// Version of the serialized BPP scene and render schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchemaVersion(u8);

impl SchemaVersion {
    /// Returns the currently supported schema version.
    #[must_use]
    pub const fn current() -> Self {
        Self(1)
    }
}

/// Error returned when a schema version is not supported by this crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSchemaVersionError {
    found: u8,
}

impl InvalidSchemaVersionError {
    /// Returns the unsupported version received from the wire format.
    #[must_use]
    pub const fn found(self) -> u8 {
        self.found
    }
}

impl fmt::Display for InvalidSchemaVersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unsupported schema version {}", self.found)
    }
}

impl std::error::Error for InvalidSchemaVersionError {}

impl TryFrom<u8> for SchemaVersion {
    type Error = InvalidSchemaVersionError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == Self::current().0 {
            Ok(Self::current())
        } else {
            Err(InvalidSchemaVersionError { found: value })
        }
    }
}

impl Serialize for SchemaVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        u8::deserialize(deserializer)?
            .try_into()
            .map_err(D::Error::custom)
    }
}

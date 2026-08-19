use std::num::NonZeroU16;

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

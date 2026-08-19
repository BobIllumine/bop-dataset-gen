use std::num::NonZeroU16;

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

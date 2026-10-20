use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::AssetId;

/// A checked value and its finite, nonnegative relative choice weight.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WeightedValue<T> {
    value: T,
    weight: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WeightedValueWire<T> {
    value: T,
    weight: f64,
}

impl<T> WeightedValue<T> {
    /// Creates a weighted value after checking its relative weight.
    ///
    /// Zero weight is valid. A containing [`Distribution::try_choice`]
    /// requires at least one entry whose weight is strictly positive.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidWeightedValueError`] when `weight` is non-finite or
    /// negative.
    pub fn try_new(value: T, weight: f64) -> Result<Self, InvalidWeightedValueError> {
        if !weight.is_finite() {
            return Err(InvalidWeightedValueError::NonFiniteWeight { weight });
        }
        if weight < 0.0 {
            return Err(InvalidWeightedValueError::NegativeWeight { weight });
        }
        Ok(Self {
            value,
            weight: canonicalize_zero_f64(weight),
        })
    }

    /// Returns the immutable choice value.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Returns the finite, nonnegative relative choice weight.
    #[must_use]
    pub const fn weight(&self) -> f64 {
        self.weight
    }
}

impl<'de, T> Deserialize<'de> for WeightedValue<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WeightedValueWire::deserialize(deserializer)?;
        Self::try_new(wire.value, wire.weight).map_err(D::Error::custom)
    }
}

/// Error returned when a choice weight is not finite and nonnegative.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidWeightedValueError {
    /// A non-finite weight was supplied.
    #[error("choice weight must be finite, got {weight}")]
    NonFiniteWeight {
        /// The rejected weight.
        weight: f64,
    },
    /// A finite negative weight was supplied.
    #[error("choice weight must be nonnegative, got {weight}")]
    NegativeWeight {
        /// The rejected weight.
        weight: f64,
    },
}

/// A validated fixed, uniform, or weighted-choice configuration value.
///
/// The public value exposes only borrowed variant data through [`Self::view`],
/// so callers cannot mutate checked bounds, values, or weights. Supported v1
/// domains are `u32`, `f32`, and [`AssetId`]. Asset distributions deliberately
/// provide no public uniform constructor.
#[derive(Clone, Debug, PartialEq)]
pub struct Distribution<T> {
    data: DistributionData<T>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DistributionData<T> {
    Fixed {
        value: T,
    },
    Uniform {
        min: T,
        max: T,
        #[serde(skip)]
        semantics: UniformSemantics,
    },
    Choice {
        values: Vec<WeightedValue<T>>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DistributionWire<T> {
    Fixed { value: T },
    Uniform { min: T, max: T },
    Choice { values: Vec<WeightedValue<T>> },
}

impl<T> Serialize for Distribution<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.data.serialize(serializer)
    }
}

impl<T> Distribution<T> {
    /// Returns a borrowed view of the distribution variant and its checked
    /// data.
    #[must_use]
    pub fn view(&self) -> DistributionView<'_, T> {
        match &self.data {
            DistributionData::Fixed { value } => DistributionView::Fixed { value },
            DistributionData::Uniform {
                min,
                max,
                semantics,
            } => DistributionView::Uniform {
                min,
                max,
                semantics: *semantics,
            },
            DistributionData::Choice { values } => DistributionView::Choice { values },
        }
    }

    fn fixed_unchecked(value: T) -> Self {
        Self {
            data: DistributionData::Fixed { value },
        }
    }

    fn uniform_unchecked(min: T, max: T, semantics: UniformSemantics) -> Self {
        Self {
            data: DistributionData::Uniform {
                min,
                max,
                semantics,
            },
        }
    }

    fn try_choice_common(values: Vec<WeightedValue<T>>) -> Result<Self, InvalidDistributionError> {
        if values.is_empty() {
            return Err(InvalidDistributionError::EmptyChoice);
        }
        if values.iter().all(|entry| entry.weight == 0.0) {
            return Err(InvalidDistributionError::AllZeroChoice);
        }
        Ok(Self {
            data: DistributionData::Choice { values },
        })
    }
}

impl Distribution<u32> {
    /// Creates a fixed unsigned-integer distribution.
    #[must_use]
    pub fn fixed(value: u32) -> Self {
        Self::fixed_unchecked(value)
    }

    /// Creates a uniform unsigned-integer distribution with inclusive bounds.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidDistributionError::InvertedUniformBounds`] when
    /// `min > max`.
    pub fn try_uniform(min: u32, max: u32) -> Result<Self, InvalidDistributionError> {
        if min > max {
            return Err(InvalidDistributionError::InvertedUniformBounds);
        }
        Ok(Self::uniform_unchecked(
            min,
            max,
            UniformSemantics::Inclusive,
        ))
    }

    /// Creates a weighted-choice unsigned-integer distribution.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidDistributionError::EmptyChoice`] for no entries or
    /// [`InvalidDistributionError::AllZeroChoice`] when every weight is zero.
    pub fn try_choice(values: Vec<WeightedValue<u32>>) -> Result<Self, InvalidDistributionError> {
        Self::try_choice_common(values)
    }
}

impl Distribution<f32> {
    /// Creates a fixed finite floating-point distribution.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidDistributionError::NonFiniteValue`] when `value` is
    /// non-finite.
    pub fn try_fixed(value: f32) -> Result<Self, InvalidDistributionError> {
        validate_finite_value(value)?;
        Ok(Self::fixed_unchecked(canonicalize_zero_f32(value)))
    }

    /// Creates a finite floating-point uniform distribution.
    ///
    /// Unequal bounds describe a closed-open interval `[min, max)`. Equal
    /// bounds describe the fixed value `min` while preserving the `uniform`
    /// wire variant.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidDistributionError::NonFiniteUniformBound`] for a
    /// non-finite bound or [`InvalidDistributionError::InvertedUniformBounds`]
    /// when `min > max`.
    pub fn try_uniform(min: f32, max: f32) -> Result<Self, InvalidDistributionError> {
        validate_finite_bound(min, UniformBound::Minimum)?;
        validate_finite_bound(max, UniformBound::Maximum)?;
        if min > max {
            return Err(InvalidDistributionError::InvertedUniformBounds);
        }
        let min = canonicalize_zero_f32(min);
        let max = canonicalize_zero_f32(max);
        let semantics = if min.to_bits() == max.to_bits() {
            UniformSemantics::Fixed
        } else {
            UniformSemantics::ClosedOpen
        };
        Ok(Self::uniform_unchecked(min, max, semantics))
    }

    /// Creates a weighted-choice finite floating-point distribution.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidDistributionError::NonFiniteValue`] when an entry
    /// value is non-finite, [`InvalidDistributionError::EmptyChoice`] for no
    /// entries, or [`InvalidDistributionError::AllZeroChoice`] when every
    /// weight is zero.
    pub fn try_choice(
        mut values: Vec<WeightedValue<f32>>,
    ) -> Result<Self, InvalidDistributionError> {
        for entry in &mut values {
            validate_finite_value(entry.value)?;
            entry.value = canonicalize_zero_f32(entry.value);
        }
        Self::try_choice_common(values)
    }
}

impl Distribution<AssetId> {
    /// Creates a fixed asset distribution.
    #[must_use]
    pub fn fixed(value: AssetId) -> Self {
        Self::fixed_unchecked(value)
    }

    /// Creates a weighted-choice asset distribution.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidDistributionError::EmptyChoice`] for no entries or
    /// [`InvalidDistributionError::AllZeroChoice`] when every weight is zero.
    pub fn try_choice(
        values: Vec<WeightedValue<AssetId>>,
    ) -> Result<Self, InvalidDistributionError> {
        Self::try_choice_common(values)
    }
}

impl<'de> Deserialize<'de> for Distribution<u32> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match DistributionWire::deserialize(deserializer)? {
            DistributionWire::Fixed { value } => Ok(Self::fixed(value)),
            DistributionWire::Uniform { min, max } => {
                Self::try_uniform(min, max).map_err(D::Error::custom)
            }
            DistributionWire::Choice { values } => {
                Self::try_choice(values).map_err(D::Error::custom)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Distribution<f32> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match DistributionWire::deserialize(deserializer)? {
            DistributionWire::Fixed { value } => Self::try_fixed(value).map_err(D::Error::custom),
            DistributionWire::Uniform { min, max } => {
                Self::try_uniform(min, max).map_err(D::Error::custom)
            }
            DistributionWire::Choice { values } => {
                Self::try_choice(values).map_err(D::Error::custom)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Distribution<AssetId> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match DistributionWire::deserialize(deserializer)? {
            DistributionWire::Fixed { value } => Ok(Self::fixed(value)),
            DistributionWire::Uniform { .. } => {
                Err(D::Error::custom(InvalidDistributionError::UniformAsset))
            }
            DistributionWire::Choice { values } => {
                Self::try_choice(values).map_err(D::Error::custom)
            }
        }
    }
}

/// A read-only borrowed view of a checked distribution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DistributionView<'a, T> {
    /// A distribution that always yields one value.
    Fixed {
        /// The fixed value.
        value: &'a T,
    },
    /// A uniform distribution over checked bounds.
    Uniform {
        /// The lower bound.
        min: &'a T,
        /// The upper bound.
        max: &'a T,
        /// The domain-specific endpoint semantics.
        semantics: UniformSemantics,
    },
    /// A distribution over immutable weighted entries.
    Choice {
        /// The nonempty entries, including at least one positive weight.
        values: &'a [WeightedValue<T>],
    },
}

/// Endpoint semantics attached to a validated uniform distribution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UniformSemantics {
    /// Both integer bounds are included.
    Inclusive,
    /// The floating-point minimum is included and maximum is excluded.
    ClosedOpen,
    /// Equal floating-point bounds represent one fixed value.
    Fixed,
}

/// Identifies a uniform bound rejected during validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UniformBound {
    /// The lower bound.
    Minimum,
    /// The upper bound.
    Maximum,
}

impl fmt::Display for UniformBound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Minimum => formatter.write_str("minimum"),
            Self::Maximum => formatter.write_str("maximum"),
        }
    }
}

/// Error returned when distribution values violate domain invariants.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidDistributionError {
    /// A fixed or weighted-choice floating-point value was non-finite.
    #[error("distribution value must be finite, got {value}")]
    NonFiniteValue {
        /// The rejected value.
        value: f32,
    },
    /// A floating-point uniform bound was non-finite.
    #[error("uniform {bound} bound must be finite, got {value}")]
    NonFiniteUniformBound {
        /// Which endpoint was rejected.
        bound: UniformBound,
        /// The rejected bound.
        value: f32,
    },
    /// No weighted entries were supplied.
    #[error("choice distribution must contain at least one value")]
    EmptyChoice,
    /// Every supplied weighted entry had zero weight.
    #[error("choice distribution must contain at least one strictly positive weight")]
    AllZeroChoice,
    /// A uniform minimum was larger than its maximum.
    #[error("uniform minimum must be less than or equal to maximum")]
    InvertedUniformBounds,
    /// A uniform asset distribution appeared on the wire.
    #[error("asset distributions support only fixed and choice variants")]
    UniformAsset,
}

fn validate_finite_value(value: f32) -> Result<(), InvalidDistributionError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(InvalidDistributionError::NonFiniteValue { value })
    }
}

fn validate_finite_bound(value: f32, bound: UniformBound) -> Result<(), InvalidDistributionError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(InvalidDistributionError::NonFiniteUniformBound { bound, value })
    }
}

const fn canonicalize_zero_f32(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

const fn canonicalize_zero_f64(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

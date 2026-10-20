use std::num::NonZeroU32;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{CameraExtrinsics, CameraId, PositiveMeters, SchemaVersion};

/// A nonzero image resolution in pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ImageSize {
    width: NonZeroU32,
    height: NonZeroU32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageSizeWire {
    width: u32,
    height: u32,
}

impl<'de> Deserialize<'de> for ImageSize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ImageSizeWire::deserialize(deserializer)?;
        Self::try_new(wire.width, wire.height).map_err(D::Error::custom)
    }
}

impl ImageSize {
    /// Creates an image size from nonzero pixel dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidImageSizeError`] when either dimension is zero.
    pub fn try_new(width: u32, height: u32) -> Result<Self, InvalidImageSizeError> {
        let width = NonZeroU32::new(width).ok_or(InvalidImageSizeError::ZeroWidth)?;
        let height = NonZeroU32::new(height).ok_or(InvalidImageSizeError::ZeroHeight)?;
        Ok(Self { width, height })
    }

    /// Returns the nonzero image width in pixels.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width.get()
    }

    /// Returns the nonzero image height in pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height.get()
    }
}

/// Error returned when an image resolution has a zero dimension.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum InvalidImageSizeError {
    /// The image width was zero.
    #[error("image width must be nonzero")]
    ZeroWidth,
    /// The image height was zero.
    #[error("image height must be nonzero")]
    ZeroHeight,
}

/// A validated row-major `OpenCV` 3x3 intrinsic calibration matrix.
///
/// The matrix is `[fx, skew, cx, 0, fy, cy, 0, 0, 1]`. It maps points in
/// `OpenCV` camera coordinates (+X right, +Y down, +Z forward), while the
/// canonical world frame is right-handed and Z-up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraIntrinsics {
    k: [f32; 9],
}

impl CameraIntrinsics {
    /// Creates intrinsics from a finite row-major `OpenCV` 3x3 calibration matrix.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidCameraIntrinsicsError`] when a value is non-finite, a
    /// focal length is not strictly positive, or a structural entry differs
    /// from its required value.
    pub fn try_new(k: [f32; 9]) -> Result<Self, InvalidCameraIntrinsicsError> {
        for (index, value) in k.into_iter().enumerate() {
            if !value.is_finite() {
                return Err(InvalidCameraIntrinsicsError::NonFiniteEntry { index, value });
            }
        }
        if k[0] <= 0.0 {
            return Err(InvalidCameraIntrinsicsError::NonPositiveFx { value: k[0] });
        }
        if k[4] <= 0.0 {
            return Err(InvalidCameraIntrinsicsError::NonPositiveFy { value: k[4] });
        }
        for (index, expected) in [(3, 0.0_f32), (6, 0.0), (7, 0.0), (8, 1.0)] {
            if k[index].to_bits() != expected.to_bits() {
                return Err(InvalidCameraIntrinsicsError::InvalidStructuralEntry {
                    index,
                    expected,
                    value: k[index],
                });
            }
        }
        Ok(Self { k })
    }

    /// Returns the complete row-major `OpenCV` 3x3 calibration matrix.
    #[must_use]
    pub const fn k(self) -> [f32; 9] {
        self.k
    }

    /// Returns the strictly positive horizontal focal length in pixels.
    #[must_use]
    pub const fn fx(self) -> f32 {
        self.k[0]
    }

    /// Returns the skew coefficient in pixels.
    #[must_use]
    pub const fn skew(self) -> f32 {
        self.k[1]
    }

    /// Returns the horizontal principal-point coordinate in pixels.
    #[must_use]
    pub const fn cx(self) -> f32 {
        self.k[2]
    }

    /// Returns the strictly positive vertical focal length in pixels.
    #[must_use]
    pub const fn fy(self) -> f32 {
        self.k[4]
    }

    /// Returns the vertical principal-point coordinate in pixels.
    #[must_use]
    pub const fn cy(self) -> f32 {
        self.k[5]
    }
}

impl Serialize for CameraIntrinsics {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.k.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CameraIntrinsics {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_new(<[f32; 9]>::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Error returned when a camera calibration matrix violates its invariants.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidCameraIntrinsicsError {
    /// The matrix entry at `index` was not finite.
    #[error("intrinsics k[{index}] must be finite, got {value}")]
    NonFiniteEntry {
        /// Zero-based index of the rejected matrix entry.
        index: usize,
        /// Rejected matrix value.
        value: f32,
    },
    /// The horizontal focal length was not strictly positive.
    #[error("intrinsics fx must be finite and greater than zero, got {value}")]
    NonPositiveFx {
        /// Rejected horizontal focal length.
        value: f32,
    },
    /// The vertical focal length was not strictly positive.
    #[error("intrinsics fy must be finite and greater than zero, got {value}")]
    NonPositiveFy {
        /// Rejected vertical focal length.
        value: f32,
    },
    /// A required homogeneous-matrix entry differed from its fixed value.
    #[error("intrinsics k[{index}] must equal {expected}, got {value}")]
    InvalidStructuralEntry {
        /// Zero-based index of the rejected matrix entry.
        index: usize,
        /// Required fixed value.
        expected: f32,
        /// Rejected matrix value.
        value: f32,
    },
}

/// An immutable, versioned camera calibration and extrinsics snapshot.
///
/// `camera_from_world` maps the canonical right-handed Z-up world coordinates
/// into `OpenCV` camera coordinates (+X right, +Y down, +Z forward).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CameraSnapshot {
    schema_version: SchemaVersion,
    camera_id: CameraId,
    image_size: ImageSize,
    k: CameraIntrinsics,
    camera_from_world: CameraExtrinsics,
    near_m: PositiveMeters,
    far_m: PositiveMeters,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CameraSnapshotWire {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersion,
    camera_id: CameraId,
    image_size: ImageSize,
    k: CameraIntrinsics,
    camera_from_world: CameraExtrinsics,
    near_m: PositiveMeters,
    far_m: PositiveMeters,
}

impl<'de> Deserialize<'de> for CameraSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CameraSnapshotWire::deserialize(deserializer)?;
        Self::try_new(
            wire.camera_id,
            wire.image_size,
            wire.k,
            wire.camera_from_world,
            wire.near_m,
            wire.far_m,
        )
        .map_err(D::Error::custom)
    }
}

impl CameraSnapshot {
    /// Creates a camera snapshot after validating its near and far clipping planes.
    ///
    /// The supplied calibration describes `OpenCV` camera coordinates (+X
    /// right, +Y down, +Z forward) relative to the canonical right-handed
    /// Z-up world frame.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidCameraSnapshotError`] when `near_m` is not strictly
    /// less than `far_m`.
    pub fn try_new(
        camera_id: CameraId,
        image_size: ImageSize,
        k: CameraIntrinsics,
        camera_from_world: CameraExtrinsics,
        near_m: PositiveMeters,
        far_m: PositiveMeters,
    ) -> Result<Self, InvalidCameraSnapshotError> {
        if near_m.get() >= far_m.get() {
            return Err(InvalidCameraSnapshotError::NearNotBeforeFar { near_m, far_m });
        }
        Ok(Self {
            schema_version: SchemaVersion::current(),
            camera_id,
            image_size,
            k,
            camera_from_world,
            near_m,
            far_m,
        })
    }

    /// Returns the wire-schema version.
    #[must_use]
    pub const fn schema_version(self) -> SchemaVersion {
        self.schema_version
    }

    /// Returns the stable camera identifier.
    #[must_use]
    pub const fn camera_id(self) -> CameraId {
        self.camera_id
    }

    /// Returns the camera image resolution in pixels.
    #[must_use]
    pub const fn image_size(self) -> ImageSize {
        self.image_size
    }

    /// Returns the checked `OpenCV` 3x3 intrinsic calibration matrix.
    #[must_use]
    pub const fn k(self) -> CameraIntrinsics {
        self.k
    }

    /// Returns the checked `OpenCV` intrinsic calibration matrix.
    #[must_use]
    pub const fn intrinsics(self) -> CameraIntrinsics {
        self.k
    }

    /// Returns the transform from canonical world to `OpenCV` camera coordinates.
    #[must_use]
    pub const fn camera_from_world(self) -> CameraExtrinsics {
        self.camera_from_world
    }

    /// Returns the strictly positive near clipping distance in metres.
    #[must_use]
    pub const fn near_m(self) -> PositiveMeters {
        self.near_m
    }

    /// Returns the strictly positive far clipping distance in metres.
    #[must_use]
    pub const fn far_m(self) -> PositiveMeters {
        self.far_m
    }
}

/// Error returned when camera clipping planes are not ordered front to back.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum InvalidCameraSnapshotError {
    /// The near clipping distance was greater than or equal to the far distance.
    #[error("near_m ({near_m:?}) must be strictly less than far_m ({far_m:?})")]
    NearNotBeforeFar {
        /// Rejected near clipping distance in metres.
        near_m: PositiveMeters,
        /// Rejected far clipping distance in metres.
        far_m: PositiveMeters,
    },
}

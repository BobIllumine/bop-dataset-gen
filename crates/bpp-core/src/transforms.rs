use glam::{Quat, Vec3, Vec4};

/// A finite signed distance measured in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Meters(f32);

impl Meters {
    /// Creates a distance when `value` is finite.
    #[must_use]
    pub fn new(value: f32) -> Option<Self> {
        value.is_finite().then_some(Self(canonicalize_zero(value)))
    }

    /// Returns this distance as a scalar number of metres.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    const fn from_finite(value: f32) -> Self {
        Self(canonicalize_zero(value))
    }
}

/// A finite, canonical rigid transform with metre translation and unit rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RigidTransform {
    translation: [f32; 3],
    rotation_xyzw: [f32; 4],
}

impl RigidTransform {
    /// Returns the transform that leaves every point unchanged.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            translation: [0.0, 0.0, 0.0],
            rotation_xyzw: [0.0, 0.0, 0.0, 1.0],
        }
    }

    /// Creates a transform from metre translation and `[x, y, z, w]` rotation.
    ///
    /// All components must be finite, and the rotation must be nonzero. Valid
    /// rotations are normalized and assigned their canonical sign.
    #[must_use]
    pub fn try_new(translation: [f32; 3], rotation_xyzw: [f32; 4]) -> Option<Self> {
        if !translation.into_iter().all(f32::is_finite) {
            return None;
        }

        let rotation_xyzw = normalize_rotation(rotation_xyzw)?;
        Some(Self {
            translation: translation.map(canonicalize_zero),
            rotation_xyzw,
        })
    }

    /// Returns the translation as finite distances in metres.
    #[must_use]
    pub fn translation(self) -> [Meters; 3] {
        self.translation.map(Meters::from_finite)
    }

    /// Returns the normalized canonical rotation in serialized `[x, y, z, w]` order.
    #[must_use]
    pub const fn rotation_xyzw(self) -> [f32; 4] {
        self.rotation_xyzw
    }

    /// Applies this transform to a point measured in metres.
    ///
    /// Returns `None` if the transformed point cannot be represented with
    /// finite metre components.
    #[must_use]
    pub fn transform_point(self, point: [Meters; 3]) -> Option<[Meters; 3]> {
        let transformed = self.rotation() * Vec3::from_array(point.map(Meters::get))
            + Vec3::from_array(self.translation);
        let transformed = transformed.to_array();
        Some([
            Meters::new(transformed[0])?,
            Meters::new(transformed[1])?,
            Meters::new(transformed[2])?,
        ])
    }

    /// Returns the inverse transform, or `None` if its finite result cannot be represented.
    #[must_use]
    pub fn inverse(self) -> Option<Self> {
        let inverse_rotation = self.rotation().conjugate();
        let inverse_translation = -(inverse_rotation * Vec3::from_array(self.translation));
        Self::try_new(inverse_translation.to_array(), inverse_rotation.to_array())
    }

    /// Composes two transforms, applying `other` first and then `self`.
    ///
    /// Returns `None` when the finite result cannot be represented.
    #[must_use]
    pub fn compose(self, other: Self) -> Option<Self> {
        let rotation = self.rotation() * other.rotation();
        let translation = self.rotation() * Vec3::from_array(other.translation)
            + Vec3::from_array(self.translation);
        Self::try_new(translation.to_array(), rotation.to_array())
    }

    fn rotation(self) -> Quat {
        Quat::from_array(self.rotation_xyzw)
    }
}

/// Returns the fixed basis transform from a local right-handed Z-up camera
/// frame into `OpenCV` camera coordinates.
///
/// The source frame has +X right, +Y forward, and +Z up. `OpenCV` coordinates
/// have +X right, +Y down, and +Z forward, so this maps `[x, y, z]` to
/// `[x, -z, y]` without translation.
///
/// # Panics
///
/// Panics if the fixed finite nonzero rotation cannot be constructed. This
/// indicates a defect in this crate.
#[must_use]
pub fn z_up_camera_to_opencv() -> RigidTransform {
    RigidTransform::try_new([0.0, 0.0, 0.0], [0.5_f32.sqrt(), 0.0, 0.0, 0.5_f32.sqrt()])
        .expect("fixed Z-up-to-OpenCV basis is a finite nonzero rotation")
}

/// Camera pose conversions between canonical world coordinates and `OpenCV`
/// camera coordinates.
///
/// The stored transform maps a world point directly into the `OpenCV` camera
/// frame, whose axes are +X right, +Y down, and +Z forward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraExtrinsics {
    camera_from_world: RigidTransform,
}

impl CameraExtrinsics {
    /// Creates camera extrinsics from a transform mapping world coordinates to
    /// `OpenCV` camera coordinates.
    #[must_use]
    pub const fn new(camera_from_world: RigidTransform) -> Self {
        Self { camera_from_world }
    }

    /// Creates camera extrinsics from a world transform of a local Z-up camera.
    ///
    /// `world_from_z_up_camera` maps from a local right-handed Z-up camera
    /// frame (+X right, +Y forward, +Z up) into canonical world coordinates.
    /// The result maps world points directly into `OpenCV` camera coordinates.
    /// Returns `None` when inversion or composition cannot produce a finite
    /// rigid transform.
    #[must_use]
    pub fn try_from_world_from_z_up_camera(world_from_z_up_camera: RigidTransform) -> Option<Self> {
        let z_up_camera_from_world = world_from_z_up_camera.inverse()?;
        let camera_from_world = z_up_camera_to_opencv().compose(z_up_camera_from_world)?;
        Some(Self::new(camera_from_world))
    }

    /// Returns the transform mapping canonical world coordinates to `OpenCV`
    /// camera coordinates.
    #[must_use]
    pub const fn camera_from_world(self) -> RigidTransform {
        self.camera_from_world
    }

    /// Returns the transform mapping `OpenCV` camera coordinates to canonical
    /// world coordinates.
    ///
    /// Returns `None` if the inverse cannot be represented with finite values.
    #[must_use]
    pub fn world_from_camera(self) -> Option<RigidTransform> {
        self.camera_from_world.inverse()
    }

    /// Converts a canonical world point into `OpenCV` camera coordinates.
    ///
    /// Returns `None` if the transformed point cannot be represented with
    /// finite metre components.
    #[must_use]
    pub fn world_to_camera(self, world_point: [Meters; 3]) -> Option<[Meters; 3]> {
        self.camera_from_world.transform_point(world_point)
    }

    /// Converts an `OpenCV` camera point into canonical world coordinates.
    ///
    /// Returns `None` if either the inverse transform or transformed point
    /// cannot be represented with finite metre components.
    #[must_use]
    pub fn camera_to_world(self, camera_point: [Meters; 3]) -> Option<[Meters; 3]> {
        self.world_from_camera()?.transform_point(camera_point)
    }
}

fn normalize_rotation(rotation_xyzw: [f32; 4]) -> Option<[f32; 4]> {
    if !rotation_xyzw.into_iter().all(f32::is_finite) {
        return None;
    }

    let scale = rotation_xyzw
        .into_iter()
        .map(f32::abs)
        .fold(0.0_f32, f32::max);
    if scale == 0.0 {
        return None;
    }

    let scaled = Vec4::from_array(rotation_xyzw.map(|component| component / scale));
    let normalized = scaled / scaled.length();
    let mut result = normalized.to_array();

    for component in [result[3], result[0], result[1], result[2]] {
        if component != 0.0 {
            if component.is_sign_negative() {
                result = result.map(|value| -value);
            }
            break;
        }
    }

    Some(result.map(canonicalize_zero))
}

const fn canonicalize_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

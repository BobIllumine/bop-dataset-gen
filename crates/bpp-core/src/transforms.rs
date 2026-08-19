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

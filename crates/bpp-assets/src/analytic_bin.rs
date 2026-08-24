use bpp_core::{BinConfig, PositiveMeters, RigidTransform};
use thiserror::Error;

/// One checked cuboid expressed in bin-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollisionCuboid {
    half_extents_m: [PositiveMeters; 3],
    bin_from_cuboid: RigidTransform,
}

impl CollisionCuboid {
    /// Returns positive `[x, y, z]` half-extents in metres.
    #[must_use]
    pub const fn half_extents_m(self) -> [PositiveMeters; 3] {
        self.half_extents_m
    }

    /// Returns the cuboid-to-bin-local rigid transform.
    #[must_use]
    pub const fn bin_from_cuboid(self) -> RigidTransform {
        self.bin_from_cuboid
    }
}

/// The five named cuboids forming an open-top bin collision asset.
///
/// Cuboids are expressed in bin-local coordinates. The inner X/Y volume is
/// centered at zero, the inner floor surface is at Z = 0, and usable height
/// extends through positive Z.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalyticBinCollision {
    floor: CollisionCuboid,
    negative_x: CollisionCuboid,
    positive_x: CollisionCuboid,
    negative_y: CollisionCuboid,
    positive_y: CollisionCuboid,
}

impl AnalyticBinCollision {
    /// Returns the floor cuboid.
    #[must_use]
    pub const fn floor(self) -> CollisionCuboid {
        self.floor
    }

    /// Returns the wall outside the negative-X inner face.
    #[must_use]
    pub const fn negative_x(self) -> CollisionCuboid {
        self.negative_x
    }

    /// Returns the wall outside the positive-X inner face.
    #[must_use]
    pub const fn positive_x(self) -> CollisionCuboid {
        self.positive_x
    }

    /// Returns the wall outside the negative-Y inner face.
    #[must_use]
    pub const fn negative_y(self) -> CollisionCuboid {
        self.negative_y
    }

    /// Returns the wall outside the positive-Y inner face.
    #[must_use]
    pub const fn positive_y(self) -> CollisionCuboid {
        self.positive_y
    }

    /// Returns all cuboids in stable floor, -X, +X, -Y, +Y order.
    #[must_use]
    pub const fn cuboids(self) -> [CollisionCuboid; 5] {
        [
            self.floor,
            self.negative_x,
            self.positive_x,
            self.negative_y,
            self.positive_y,
        ]
    }
}

/// Error returned when derived analytic-bin geometry is not representable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AnalyticBinCollisionError {
    /// A derived half-extent or pose component overflowed finite `f32` metres.
    #[error("derived analytic-bin collision geometry is not finite")]
    DerivedGeometryNotFinite,
}

/// Derives the five collision cuboids of an open-top bin in bin-local space.
///
/// The floor covers the complete outer footprint. X walls span the outer Y
/// dimension and Y walls span the inner X dimension, tiling the four outer
/// corners without overlapping wall volumes. The bin's world pose and contact
/// properties are deliberately not embedded in the returned geometry.
///
/// # Errors
///
/// Returns [`AnalyticBinCollisionError::DerivedGeometryNotFinite`] if finite
/// checked configuration values overflow while deriving cuboid geometry.
pub fn analytic_bin_collision(
    bin: &BinConfig,
) -> Result<AnalyticBinCollision, AnalyticBinCollisionError> {
    let [x, y, z] = bin.inner_dimensions_m().map(PositiveMeters::get);
    let floor = bin.floor_thickness_m().get();
    let wall = bin.wall_thickness_m().get();
    let half_x = finite(x / 2.0)?;
    let half_y = finite(y / 2.0)?;
    let half_z = finite(z / 2.0)?;
    let half_floor = finite(floor / 2.0)?;
    let half_wall = finite(wall / 2.0)?;
    let outer_half_x = finite(half_x + wall)?;
    let outer_half_y = finite(half_y + wall)?;
    let wall_center_x = finite(half_x + half_wall)?;
    let wall_center_y = finite(half_y + half_wall)?;

    Ok(AnalyticBinCollision {
        floor: cuboid(
            [outer_half_x, outer_half_y, half_floor],
            [0.0, 0.0, canonicalize_zero(-half_floor)],
        )?,
        negative_x: cuboid(
            [half_wall, outer_half_y, half_z],
            [-wall_center_x, 0.0, half_z],
        )?,
        positive_x: cuboid(
            [half_wall, outer_half_y, half_z],
            [wall_center_x, 0.0, half_z],
        )?,
        negative_y: cuboid([half_x, half_wall, half_z], [0.0, -wall_center_y, half_z])?,
        positive_y: cuboid([half_x, half_wall, half_z], [0.0, wall_center_y, half_z])?,
    })
}

fn cuboid(
    half_extents_m: [f32; 3],
    translation_m: [f32; 3],
) -> Result<CollisionCuboid, AnalyticBinCollisionError> {
    let half_extents_m = half_extents_m
        .map(PositiveMeters::try_new)
        .map(|result| result.map_err(|_| AnalyticBinCollisionError::DerivedGeometryNotFinite));
    let [x, y, z] = half_extents_m;
    let bin_from_cuboid =
        RigidTransform::try_new(translation_m.map(canonicalize_zero), [0.0, 0.0, 0.0, 1.0])
            .ok_or(AnalyticBinCollisionError::DerivedGeometryNotFinite)?;
    Ok(CollisionCuboid {
        half_extents_m: [x?, y?, z?],
        bin_from_cuboid,
    })
}

fn finite(value: f32) -> Result<f32, AnalyticBinCollisionError> {
    value
        .is_finite()
        .then(|| canonicalize_zero(value))
        .ok_or(AnalyticBinCollisionError::DerivedGeometryNotFinite)
}

const fn canonicalize_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

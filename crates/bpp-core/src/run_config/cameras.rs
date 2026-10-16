//! Camera projection, pose, and scene appearance settings.

#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the parent's imports and helpers"
)]
use super::*;

/// A checked vertical field of view in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VerticalFovDegrees(pub(super) f32);

impl VerticalFovDegrees {
    /// Returns the finite value strictly between 0 and 180 degrees.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// Exactly one checked camera projection source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraProjection {
    /// An explicit checked row-major `OpenCV` calibration matrix.
    OpenCvK(CameraIntrinsics),
    /// A vertical field of view strictly between 0 and 180 degrees.
    VerticalFov(VerticalFovDegrees),
}

/// A checked finite shell from which a camera pose may later be sampled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShellCameraPose {
    pub(super) min_radius_m: PositiveMeters,
    pub(super) max_radius_m: PositiveMeters,
    pub(super) min_azimuth_deg: f32,
    pub(super) max_azimuth_deg: f32,
    pub(super) min_elevation_deg: f32,
    pub(super) max_elevation_deg: f32,
    pub(super) target_bin_m: [Meters; 3],
}

impl ShellCameraPose {
    /// Returns the positive minimum shell radius.
    #[must_use]
    pub const fn min_radius_m(self) -> PositiveMeters {
        self.min_radius_m
    }
    /// Returns the positive maximum shell radius.
    #[must_use]
    pub const fn max_radius_m(self) -> PositiveMeters {
        self.max_radius_m
    }
    /// Returns the finite minimum azimuth in degrees.
    #[must_use]
    pub const fn min_azimuth_deg(self) -> f32 {
        self.min_azimuth_deg
    }
    /// Returns the finite maximum azimuth in degrees.
    #[must_use]
    pub const fn max_azimuth_deg(self) -> f32 {
        self.max_azimuth_deg
    }
    /// Returns the finite minimum elevation in degrees.
    #[must_use]
    pub const fn min_elevation_deg(self) -> f32 {
        self.min_elevation_deg
    }
    /// Returns the finite maximum elevation in degrees.
    #[must_use]
    pub const fn max_elevation_deg(self) -> f32 {
        self.max_elevation_deg
    }
    /// Returns the finite shell target in bin-local metres.
    #[must_use]
    pub const fn target_bin_m(self) -> [Meters; 3] {
        self.target_bin_m
    }
}

/// Exactly one checked camera pose source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraPose {
    /// A fixed transform from a local right-handed Z-up camera frame to world.
    ///
    /// The local frame uses +X right, +Y forward, and +Z up, matching the
    /// input convention accepted by [`crate::CameraExtrinsics::try_from_world_from_z_up_camera`].
    FixedWorldFromCamera(RigidTransform),
    /// A finite shell specification for later deterministic sampling.
    Shell(ShellCameraPose),
}

/// Immutable checked camera configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraConfig {
    pub(super) id: CameraId,
    pub(super) image_size: ImageSize,
    pub(super) near_m: PositiveMeters,
    pub(super) far_m: PositiveMeters,
    pub(super) projection: CameraProjection,
    pub(super) pose: CameraPose,
}

impl CameraConfig {
    /// Returns the stable camera ID.
    #[must_use]
    pub const fn id(self) -> CameraId {
        self.id
    }
    /// Returns the nonzero image size.
    #[must_use]
    pub const fn image_size(self) -> ImageSize {
        self.image_size
    }
    /// Returns the positive near clipping distance.
    #[must_use]
    pub const fn near_m(self) -> PositiveMeters {
        self.near_m
    }
    /// Returns the positive far clipping distance.
    #[must_use]
    pub const fn far_m(self) -> PositiveMeters {
        self.far_m
    }
    /// Returns the sole projection source.
    #[must_use]
    pub const fn projection(self) -> CameraProjection {
        self.projection
    }
    /// Returns the sole pose source.
    #[must_use]
    pub const fn pose(self) -> CameraPose {
        self.pose
    }

    pub(super) fn try_from_wire(wire: CameraWire) -> Result<Self, RunConfigValidationError> {
        let id = CameraId::new(wire.id);
        let image_size = checked_image_size(wire.image_size, id)?;
        let near_m = positive_meters(wire.near_m, ConfigScalarField::CameraNear)
            .map_err(|_| RunConfigValidationError::InvalidCameraClipping { camera_id: id })?;
        let far_m = positive_meters(wire.far_m, ConfigScalarField::CameraFar)
            .map_err(|_| RunConfigValidationError::InvalidCameraClipping { camera_id: id })?;
        if near_m.get() >= far_m.get() {
            return Err(RunConfigValidationError::InvalidCameraClipping { camera_id: id });
        }
        let projection = checked_projection(wire.projection, id)?;
        let pose = checked_pose(wire.pose, id)?;
        Ok(Self {
            id,
            image_size,
            near_m,
            far_m,
            projection,
            pose,
        })
    }
}

/// Checked appearance snapshot and PBR material defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceConfig {
    pub(super) snapshot: crate::AppearanceSnapshot,
    pub(super) material_defaults: PbrMaterialConfig,
}

impl AppearanceConfig {
    /// Returns the checked background, ambient term, and configured-order lights.
    #[must_use]
    pub const fn snapshot(&self) -> &crate::AppearanceSnapshot {
        &self.snapshot
    }
    /// Returns the checked PBR material defaults.
    #[must_use]
    pub const fn material_defaults(&self) -> &PbrMaterialConfig {
        &self.material_defaults
    }

    pub(super) fn try_from_wire(wire: AppearanceWire) -> Result<Self, RunConfigValidationError> {
        let background_srgb = SrgbColor::try_new(wire.background_srgb)
            .map_err(|_| invalid_appearance(AppearanceField::Background, None))?;
        let ambient_light = checked_ambient(wire.ambient)?;
        let directional_lights = wire
            .directional_lights
            .into_iter()
            .enumerate()
            .map(|(index, light)| checked_directional_light(light, index))
            .collect::<Result<Vec<_>, _>>()?;
        let point_lights = wire
            .point_lights
            .into_iter()
            .enumerate()
            .map(|(index, light)| checked_point_light(light, index))
            .collect::<Result<Vec<_>, _>>()?;
        let material_defaults = PbrMaterialConfig::try_from_wire(wire.material_defaults, None)?;
        Ok(Self {
            snapshot: crate::AppearanceSnapshot::new(
                background_srgb,
                ambient_light,
                directional_lights,
                point_lights,
            ),
            material_defaults,
        })
    }
}

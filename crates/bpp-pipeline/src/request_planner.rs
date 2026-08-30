use bpp_core::{
    CameraExtrinsics, CameraIntrinsics, CameraPose, CameraProjection, CameraSnapshot,
    InvalidCameraSnapshotError, InvalidRenderSceneRequestError, Meters, RenderSceneRequest,
    RigidTransform, RunPlan, SceneSnapshot, SeedDeriver, SeedStage,
};
use glam::{Mat3, Quat, Vec3};
use thiserror::Error;

const CAMERA_ADDRESS_DOMAIN: &[u8] = b"bpp.render-request.camera.v1";

/// Deterministic compiler from a checked run plan and physical scene to one render request.
pub struct RenderRequestPlanner {
    plan: RunPlan,
    seeds: SeedDeriver,
}

impl RenderRequestPlanner {
    /// Captures immutable checked camera, appearance, and output configuration.
    #[must_use]
    pub fn new(plan: &RunPlan) -> Self {
        Self {
            plan: plan.clone(),
            seeds: SeedDeriver::new(plan.run().master_seed()),
        }
    }

    /// Compiles every configured camera for the accepted scene attempt.
    ///
    /// Fixed cameras are copied exactly. Shell cameras use independently
    /// addressed radius, azimuth, and elevation samples derived from the
    /// camera-stage seed, scene ID, accepted attempt, camera ID, and component.
    ///
    /// # Errors
    ///
    /// Returns a typed error if derived geometry or a canonical DTO cannot be represented.
    pub fn plan(
        &self,
        scene: SceneSnapshot,
    ) -> Result<RenderSceneRequest, RenderRequestPlanningError> {
        let camera_seed = self.seeds.derive(
            scene.scene_id(),
            scene.accepted_attempt(),
            SeedStage::Camera,
        );
        let cameras = self
            .plan
            .cameras()
            .iter()
            .copied()
            .map(|camera| self.camera(camera, camera_seed))
            .collect::<Result<Vec<_>, _>>()?;
        RenderSceneRequest::try_new(
            scene,
            cameras,
            self.plan.appearance().snapshot().clone(),
            self.plan.output_channels(),
        )
        .map_err(RenderRequestPlanningError::Request)
    }

    fn camera(
        &self,
        camera: bpp_core::CameraConfig,
        camera_seed: [u8; 32],
    ) -> Result<CameraSnapshot, RenderRequestPlanningError> {
        let intrinsics = match camera.projection() {
            CameraProjection::OpenCvK(intrinsics) => intrinsics,
            CameraProjection::VerticalFov(vertical_fov) => {
                vertical_fov_intrinsics(camera.image_size(), vertical_fov.get())?
            }
        };
        let world_from_camera = match camera.pose() {
            CameraPose::FixedWorldFromCamera(transform) => transform,
            CameraPose::Shell(shell) => shell_pose(
                self.plan.bin().world_from_bin(),
                shell,
                camera_seed,
                camera.id().get(),
            )?,
        };
        let extrinsics = CameraExtrinsics::try_from_world_from_z_up_camera(world_from_camera)
            .ok_or(RenderRequestPlanningError::UnrepresentableCameraPose {
                camera_id: camera.id().get(),
            })?;
        CameraSnapshot::try_new(
            camera.id(),
            camera.image_size(),
            intrinsics,
            extrinsics,
            camera.near_m(),
            camera.far_m(),
        )
        .map_err(RenderRequestPlanningError::Camera)
    }
}

#[allow(clippy::cast_precision_loss)]
fn vertical_fov_intrinsics(
    size: bpp_core::ImageSize,
    vertical_fov_degrees: f32,
) -> Result<CameraIntrinsics, RenderRequestPlanningError> {
    let width = size.width() as f32;
    let height = size.height() as f32;
    let focal = height / (2.0 * (vertical_fov_degrees.to_radians() * 0.5).tan());
    CameraIntrinsics::try_new([
        focal,
        0.0,
        width * 0.5,
        0.0,
        focal,
        height * 0.5,
        0.0,
        0.0,
        1.0,
    ])
    .map_err(|_| RenderRequestPlanningError::InvalidDerivedIntrinsics)
}

fn shell_pose(
    world_from_bin: RigidTransform,
    shell: bpp_core::ShellCameraPose,
    seed: [u8; 32],
    camera_id: u16,
) -> Result<RigidTransform, RenderRequestPlanningError> {
    let radius = sample_range(
        seed,
        camera_id,
        b"radius",
        shell.min_radius_m().get(),
        shell.max_radius_m().get(),
    );
    let azimuth = sample_range(
        seed,
        camera_id,
        b"azimuth",
        shell.min_azimuth_deg(),
        shell.max_azimuth_deg(),
    )
    .to_radians();
    let elevation = sample_range(
        seed,
        camera_id,
        b"elevation",
        shell.min_elevation_deg(),
        shell.max_elevation_deg(),
    )
    .to_radians();
    let target_bin = Vec3::from_array(shell.target_bin_m().map(Meters::get));
    let offset_bin = Vec3::new(
        radius * elevation.cos() * azimuth.cos(),
        radius * elevation.cos() * azimuth.sin(),
        radius * elevation.sin(),
    );
    let target_world = transformed(world_from_bin, target_bin)?;
    let position_world = transformed(world_from_bin, target_bin + offset_bin)?;
    let forward = (target_world - position_world).normalize_or_zero();
    if forward == Vec3::ZERO {
        return Err(RenderRequestPlanningError::UnrepresentableCameraPose { camera_id });
    }
    let bin_rotation = Quat::from_array(world_from_bin.rotation_xyzw());
    let preferred_up = bin_rotation * Vec3::Z;
    let mut right = forward.cross(preferred_up);
    if right.length_squared() <= 64.0 * f32::EPSILON {
        let fallback = if forward.x.abs() < 0.9 {
            Vec3::X
        } else {
            Vec3::Y
        };
        right = forward.cross(fallback);
    }
    right = right.normalize();
    let up = right.cross(forward).normalize();
    let rotation = Quat::from_mat3(&Mat3::from_cols(right, forward, up)).normalize();
    RigidTransform::try_new(position_world.to_array(), rotation.to_array())
        .ok_or(RenderRequestPlanningError::UnrepresentableCameraPose { camera_id })
}

fn transformed(transform: RigidTransform, point: Vec3) -> Result<Vec3, RenderRequestPlanningError> {
    let point = [
        Meters::new(point.x).ok_or(RenderRequestPlanningError::UnrepresentableShellGeometry)?,
        Meters::new(point.y).ok_or(RenderRequestPlanningError::UnrepresentableShellGeometry)?,
        Meters::new(point.z).ok_or(RenderRequestPlanningError::UnrepresentableShellGeometry)?,
    ];
    transform
        .transform_point(point)
        .map(|point| Vec3::from_array(point.map(Meters::get)))
        .ok_or(RenderRequestPlanningError::UnrepresentableShellGeometry)
}

#[allow(clippy::cast_precision_loss)]
fn sample_range(seed: [u8; 32], camera_id: u16, label: &[u8], minimum: f32, maximum: f32) -> f32 {
    if minimum.to_bits() == maximum.to_bits() {
        return minimum;
    }
    let mut hasher = blake3::Hasher::new_keyed(&seed);
    hasher.update(CAMERA_ADDRESS_DOMAIN);
    hasher.update(&camera_id.to_le_bytes());
    hasher.update(label);
    let bytes = hasher.finalize();
    let raw = u32::from_le_bytes(bytes.as_bytes()[0..4].try_into().expect("four bytes")) >> 8;
    let unit = raw as f32 / 16_777_216.0;
    minimum + (maximum - minimum) * unit
}

/// Failure while compiling checked render configuration for one physical scene.
#[derive(Debug, Error)]
pub enum RenderRequestPlanningError {
    /// Vertical-FOV projection could not produce canonical finite intrinsics.
    #[error("vertical FOV produced invalid camera intrinsics")]
    InvalidDerivedIntrinsics,
    /// Shell geometry overflowed while being transformed into world coordinates.
    #[error("shell camera geometry cannot be represented in world coordinates")]
    UnrepresentableShellGeometry,
    /// A fixed or sampled camera pose could not become canonical extrinsics.
    #[error("camera {camera_id} pose cannot be represented as canonical extrinsics")]
    UnrepresentableCameraPose {
        /// Camera whose pose failed.
        camera_id: u16,
    },
    /// A canonical camera snapshot invariant failed.
    #[error(transparent)]
    Camera(#[from] InvalidCameraSnapshotError),
    /// The final request violated a canonical aggregate invariant.
    #[error(transparent)]
    Request(#[from] InvalidRenderSceneRequestError),
}

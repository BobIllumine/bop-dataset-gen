use bpp_assets::{AnalyticBinCollisionError, analytic_bin_collision};
use bpp_core::{BinConfig, RigidTransform};
use rapier3d::{
    na::{Isometry3, Quaternion, Translation3, UnitQuaternion, Vector3},
    prelude::{
        BroadPhaseBvh, CCDSolver, ColliderSet, ImpulseJointSet, IntegrationParameters,
        IslandManager, MultibodyJointSet, NarrowPhase, PhysicsPipeline, Real, RigidBodyBuilder,
        RigidBodyHandle, RigidBodySet,
    },
};
use thiserror::Error;

/// A serial Rapier world containing the fixed analytic bin.
#[allow(dead_code)]
pub struct PhysicsWorld {
    physics_pipeline: PhysicsPipeline,
    integration_parameters: IntegrationParameters,
    island_manager: IslandManager,
    broad_phase: BroadPhaseBvh,
    narrow_phase: NarrowPhase,
    rigid_bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd_solver: CCDSolver,
    gravity: Vector3<Real>,
    bin_body: RigidBodyHandle,
    bin_collider_count: usize,
}

/// Error returned when a physics world cannot be constructed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PhysicsWorldError {
    /// Deriving finite analytic bin collision geometry failed.
    #[error(transparent)]
    AnalyticBin(#[from] AnalyticBinCollisionError),
    /// A Rapier pose could not be represented as a finite core transform.
    #[error("Rapier pose cannot be represented as a finite core transform")]
    NonFiniteTransform,
}

impl PhysicsWorld {
    /// Creates a serial Rapier world containing exactly one fixed analytic bin.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsWorldError::AnalyticBin`] when the checked bin values
    /// overflow while deriving its analytic collision geometry.
    pub fn try_new(bin: &BinConfig) -> Result<Self, PhysicsWorldError> {
        let gravity = Vector3::new(0.0, 0.0, -9.81);
        let mut rigid_bodies = RigidBodySet::new();
        let mut colliders = ColliderSet::new();
        let bin_body = rigid_bodies.insert(
            RigidBodyBuilder::fixed()
                .pose(rigid_transform_to_isometry(bin.world_from_bin())?.into())
                .build(),
        );
        let physical_properties = bin.physical_properties();
        let cuboids = analytic_bin_collision(bin)?.cuboids();

        for cuboid in cuboids {
            let [x, y, z] = cuboid.half_extents_m().map(bpp_core::PositiveMeters::get);
            let collider = rapier3d::prelude::ColliderBuilder::cuboid(x, y, z)
                .position(rigid_transform_to_isometry(cuboid.bin_from_cuboid())?.into())
                .friction(physical_properties.friction())
                .restitution(physical_properties.restitution())
                .build();
            colliders.insert_with_parent(collider, bin_body, &mut rigid_bodies);
        }

        Ok(Self {
            physics_pipeline: PhysicsPipeline::new(),
            integration_parameters: IntegrationParameters::default(),
            island_manager: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            rigid_bodies,
            colliders,
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            gravity,
            bin_body,
            bin_collider_count: cuboids.len(),
        })
    }

    /// Returns the world's constant Z-up gravitational acceleration in m/s².
    #[must_use]
    pub fn gravity_m_s2(&self) -> [f32; 3] {
        [self.gravity.x, self.gravity.y, self.gravity.z]
    }

    /// Returns the number of analytic colliders attached to the fixed bin body.
    #[must_use]
    pub const fn bin_collider_count(&self) -> usize {
        self.bin_collider_count
    }
}

fn rigid_transform_to_isometry(
    transform: RigidTransform,
) -> Result<Isometry3<Real>, PhysicsWorldError> {
    let [x, y, z] = transform.translation().map(bpp_core::Meters::get);
    let [qx, qy, qz, qw] = transform.rotation_xyzw();
    if ![x, y, z, qx, qy, qz, qw].into_iter().all(f32::is_finite) {
        return Err(PhysicsWorldError::NonFiniteTransform);
    }

    Ok(Isometry3::from_parts(
        Translation3::new(x, y, z),
        UnitQuaternion::from_quaternion(Quaternion::new(qw, qx, qy, qz)),
    ))
}

#[allow(dead_code)]
fn rigid_transform_from_isometry(
    isometry: Isometry3<Real>,
) -> Result<RigidTransform, PhysicsWorldError> {
    let translation = isometry.translation.vector;
    let quaternion = isometry.rotation.quaternion();
    RigidTransform::try_new(
        [translation.x, translation.y, translation.z],
        [quaternion.i, quaternion.j, quaternion.k, quaternion.w],
    )
    .ok_or(PhysicsWorldError::NonFiniteTransform)
}

#[cfg(test)]
mod tests {
    use bpp_assets::analytic_bin_collision;
    use bpp_core::RunConfig;

    use super::{PhysicsWorld, rigid_transform_from_isometry, rigid_transform_to_isometry};

    fn bin(
        translation: [f32; 3],
        rotation_xyzw: [f32; 4],
        friction: f32,
        restitution: f32,
    ) -> bpp_core::BinConfig {
        let [tx, ty, tz] = translation;
        let [rx, ry, rz, rw] = rotation_xyzw;
        let input = format!(
            r#"[run]
master_seed = 1
valid_scene_count = 1
simulation_workers = 1
renderer_workers = 1
queue_capacity = 1
output_path = "output"
cache_path = "cache"

[bin]
inner_dimensions_m = [1.0, 0.8, 0.5]
floor_thickness_m = 0.02
wall_thickness_m = 0.03
world_from_bin = {{ translation = [{tx:e}, {ty:e}, {tz:e}], rotation_xyzw = [{rx:e}, {ry:e}, {rz:e}, {rw:e}] }}
material_label = "steel"
physical_properties = {{ friction = {friction:e}, restitution = {restitution:e} }}

[[assets]]
id = "part"
source_path = "part.glb"
source_unit_scale_m = 1.0
model_transform = {{ translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
mixture_weight = 1.0
physical_properties = {{ mass_kg = {{ kind = "fixed", value = 0.5 }}, friction = {{ kind = "fixed", value = 0.5 }}, restitution = {{ kind = "fixed", value = 0.1 }} }}
vhacd = {{ resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }}

[population]
object_count = {{ kind = "fixed", value = 1 }}
asset_mixture = {{ kind = "fixed", value = "part" }}
spawn_volume = {{ min_m = [-0.25, -0.2, 0.0], max_m = [0.25, 0.2, 0.25] }}

[physics]
substeps = 1
solver_iterations = 1

[[cameras]]
id = 1
image_size = {{ width = 16, height = 16 }}
near_m = 0.01
far_m = 10.0
projection = {{ vertical_fov_deg = 55.0 }}
pose = {{ fixed_world_from_camera = {{ translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }} }}

[appearance]
background_srgb = [0.0, 0.0, 0.0, 1.0]
ambient = {{ color = [1.0, 1.0, 1.0], intensity = 0.0 }}
directional_lights = []
point_lights = []
material_defaults = {{ base_color_srgb = [0.5, 0.5, 0.5, 1.0], metallic = {{ kind = "fixed", value = 0.0 }}, roughness = {{ kind = "fixed", value = 0.5 }} }}

[renderers.wgpu]
device_preference = "automatic"
adapter_preference = "high_performance"
shadow_map_size = 16
rgb_fxaa = false

[renderers.blenderproc]
executable_path = "blenderproc"
worker_path = "worker.py"
device_selection = "CPU"
handshake_timeout_s = 1.0
request_timeout_s = 1.0
stderr_tail_capacity = 64

[output]
rgb = true
depth = false
visible_instances = false
"#,
        );
        RunConfig::from_toml(&input)
            .expect("test configuration parses")
            .into_plan()
            .bin()
            .clone()
    }

    #[test]
    fn transform_bridge_round_trips_translation_and_xyzw_quaternion_order() {
        let transform = bpp_core::RigidTransform::try_new(
            [1.25, -2.5, 3.75],
            [0.0, 0.0, 0.5_f32.sqrt(), 0.5_f32.sqrt()],
        )
        .expect("finite nonzero transform");

        let isometry = rigid_transform_to_isometry(transform)
            .expect("core transform converts to a finite Rapier pose");
        let round_trip =
            rigid_transform_from_isometry(isometry).expect("Rapier pose remains a core transform");

        assert_eq!(round_trip, transform);
    }

    #[test]
    fn fixed_body_applies_the_bin_world_transform_once_and_keeps_cuboids_local() {
        let bin = bin(
            [10.0, -20.0, 30.0],
            [0.0, 0.0, 0.5_f32.sqrt(), 0.5_f32.sqrt()],
            0.5,
            0.1,
        );
        let expected_cuboids = analytic_bin_collision(&bin)
            .expect("finite bin derives")
            .cuboids();
        let world = PhysicsWorld::try_new(&bin).expect("finite bin constructs");

        assert_eq!(
            rigid_transform_from_isometry((*world.rigid_bodies[world.bin_body].position()).into())
                .expect("fixed body has a finite pose"),
            bin.world_from_bin()
        );
        for (collider, expected) in world
            .colliders
            .iter()
            .map(|(_, collider)| collider)
            .zip(expected_cuboids)
        {
            assert_eq!(collider.parent(), Some(world.bin_body));
            assert_eq!(
                rigid_transform_from_isometry(
                    (*collider
                        .position_wrt_parent()
                        .expect("collider is parented"))
                    .into(),
                )
                .expect("collider has a finite local pose"),
                expected.bin_from_cuboid()
            );
        }
    }

    #[test]
    fn every_analytic_collider_uses_the_configured_contact_coefficients() {
        let bin = bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.73, 0.27);
        let world = PhysicsWorld::try_new(&bin).expect("finite bin constructs");

        for collider in world.colliders.iter().map(|(_, collider)| collider) {
            assert_eq!(collider.friction().to_bits(), 0.73_f32.to_bits());
            assert_eq!(collider.restitution().to_bits(), 0.27_f32.to_bits());
        }
    }
}

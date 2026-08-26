use std::collections::{BTreeMap, BTreeSet};

use bpp_assets::{AnalyticBinCollisionError, ConvexPart, analytic_bin_collision};
use bpp_core::{AssetId, BinConfig, InstanceId, ResolvedPhysicalProperties, RigidTransform};
use rapier3d::{
    na::{Isometry3, Quaternion, Translation3, UnitQuaternion, Vector3},
    parry::shape::{ConvexPolyhedron, Shape},
    prelude::{
        BroadPhaseBvh, CCDSolver, Collider, ColliderHandle, ColliderSet, ImpulseJointSet,
        IntegrationParameters, IslandManager, MultibodyJointSet, NarrowPhase, PhysicsPipeline,
        Real, RigidBodyBuilder, RigidBodyHandle, RigidBodySet, SharedShape, Vector,
    },
};
use thiserror::Error;

/// Borrowed checked input for one dynamic rigid body.
pub struct DynamicBodySpec<'a> {
    instance_id: InstanceId,
    asset_id: &'a AssetId,
    convex_parts: &'a [ConvexPart],
    world_from_model: RigidTransform,
    physical_properties: ResolvedPhysicalProperties,
}

impl<'a> DynamicBodySpec<'a> {
    /// Creates a dynamic-body specification from checked borrowed geometry and metadata.
    #[must_use]
    pub const fn new(
        instance_id: InstanceId,
        asset_id: &'a AssetId,
        convex_parts: &'a [ConvexPart],
        world_from_model: RigidTransform,
        physical_properties: ResolvedPhysicalProperties,
    ) -> Self {
        Self {
            instance_id,
            asset_id,
            convex_parts,
            world_from_model,
            physical_properties,
        }
    }

    /// Returns the stable ID assigned to this body.
    #[must_use]
    pub const fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    /// Returns the resolved asset ID borrowed by this specification.
    #[must_use]
    pub const fn asset_id(&self) -> &AssetId {
        self.asset_id
    }

    /// Returns the checked convex collision parts in source order.
    #[must_use]
    pub const fn convex_parts(&self) -> &'a [ConvexPart] {
        self.convex_parts
    }

    /// Returns the initial transform from model coordinates to world coordinates.
    #[must_use]
    pub const fn world_from_model(&self) -> RigidTransform {
        self.world_from_model
    }

    /// Returns the resolved mass and contact properties for this body.
    #[must_use]
    pub const fn physical_properties(&self) -> ResolvedPhysicalProperties {
        self.physical_properties
    }
}

#[allow(dead_code)]
struct DynamicBodyMetadata {
    asset_id: AssetId,
    physical_properties: ResolvedPhysicalProperties,
    body_handle: RigidBodyHandle,
    collider_handles: Vec<ColliderHandle>,
}

struct PreparedDynamicBody {
    instance_id: InstanceId,
    asset_id: AssetId,
    world_from_model: Isometry3<Real>,
    physical_properties: ResolvedPhysicalProperties,
    colliders: Vec<Collider>,
}

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
    dynamic_bodies: BTreeMap<InstanceId, DynamicBodyMetadata>,
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
    /// A submitted instance ID is duplicated in the batch or already exists in the world.
    #[error("duplicate dynamic-body instance ID {instance_id:?}")]
    DuplicateInstanceId {
        /// The duplicate stable instance ID.
        instance_id: InstanceId,
    },
    /// A submitted body has no convex collision parts.
    #[error("dynamic body {instance_id:?} has no convex collision parts")]
    EmptyConvexParts {
        /// The stable ID of the body with no collision parts.
        instance_id: InstanceId,
    },
    /// Rapier could not form one checked convex part into a volumetric hull.
    #[error("dynamic body {instance_id:?} convex part {part_index} is not volumetric")]
    InvalidConvexHull {
        /// The stable ID of the body containing the rejected part.
        instance_id: InstanceId,
        /// The zero-based source-order index of the rejected convex part.
        part_index: usize,
    },
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
            dynamic_bodies: BTreeMap::new(),
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

    /// Returns the number of inserted dynamic rigid bodies.
    #[must_use]
    pub fn dynamic_body_count(&self) -> usize {
        self.dynamic_bodies.len()
    }

    /// Iterates inserted body IDs in ascending stable-ID order.
    #[must_use]
    pub fn instance_ids(&self) -> impl ExactSizeIterator<Item = InstanceId> + '_ {
        self.dynamic_bodies.keys().copied()
    }

    /// Returns an inserted body's current world pose, if its stored Rapier pose is finite.
    #[must_use]
    pub fn body_pose(&self, instance_id: InstanceId) -> Option<RigidTransform> {
        let metadata = self.dynamic_bodies.get(&instance_id)?;
        let body = self.rigid_bodies.get(metadata.body_handle)?;
        rigid_transform_from_isometry((*body.position()).into()).ok()
    }

    /// Returns the number of colliders attached to an inserted dynamic body.
    #[must_use]
    pub fn collider_count(&self, instance_id: InstanceId) -> Option<usize> {
        self.dynamic_bodies
            .get(&instance_id)
            .map(|metadata| metadata.collider_handles.len())
    }

    /// Validates and inserts a complete dynamic-body batch in stable-ID order.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the world when an ID is duplicate, a
    /// body has no collision parts, a convex hull is not volumetric, or a
    /// checked transform cannot be represented by Rapier.
    pub fn insert_dynamic_bodies(
        &mut self,
        bodies: &[DynamicBodySpec<'_>],
    ) -> Result<(), PhysicsWorldError> {
        let mut submitted_ids = BTreeSet::new();
        let mut prepared = Vec::with_capacity(bodies.len());

        for body in bodies {
            let instance_id = body.instance_id();
            if !submitted_ids.insert(instance_id) || self.dynamic_bodies.contains_key(&instance_id)
            {
                return Err(PhysicsWorldError::DuplicateInstanceId { instance_id });
            }
            if body.convex_parts().is_empty() {
                return Err(PhysicsWorldError::EmptyConvexParts { instance_id });
            }

            let mut colliders = Vec::with_capacity(body.convex_parts().len());
            for (part_index, part) in body.convex_parts().iter().enumerate() {
                let vertices = part
                    .vertices()
                    .iter()
                    .map(|&[x, y, z]| Vector::new(x, y, z))
                    .collect::<Vec<_>>();
                let hull = ConvexPolyhedron::from_convex_hull(&vertices)
                    .filter(|hull| {
                        let mass = hull.mass_properties(1.0).mass();
                        mass.is_finite() && mass > 0.0
                    })
                    .ok_or(PhysicsWorldError::InvalidConvexHull {
                        instance_id,
                        part_index,
                    })?;
                let collider = rapier3d::prelude::ColliderBuilder::new(SharedShape::new(hull))
                    .density(0.0)
                    .friction(body.physical_properties().friction())
                    .restitution(body.physical_properties().restitution())
                    .build();
                colliders.push(collider);
            }

            prepared.push(PreparedDynamicBody {
                instance_id,
                asset_id: body.asset_id().clone(),
                world_from_model: rigid_transform_to_isometry(body.world_from_model())?,
                physical_properties: body.physical_properties(),
                colliders,
            });
        }

        prepared.sort_unstable_by_key(|body| body.instance_id);
        for body in prepared {
            let body_handle = self.rigid_bodies.insert(
                RigidBodyBuilder::dynamic()
                    .pose(body.world_from_model.into())
                    .additional_mass(body.physical_properties.mass_kg())
                    .build(),
            );
            let collider_handles = body
                .colliders
                .into_iter()
                .map(|collider| {
                    self.colliders
                        .insert_with_parent(collider, body_handle, &mut self.rigid_bodies)
                })
                .collect();
            self.rigid_bodies[body_handle]
                .recompute_mass_properties_from_colliders(&self.colliders);
            self.dynamic_bodies.insert(
                body.instance_id,
                DynamicBodyMetadata {
                    asset_id: body.asset_id,
                    physical_properties: body.physical_properties,
                    body_handle,
                    collider_handles,
                },
            );
        }

        Ok(())
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
    use bpp_assets::{ConvexPart, analytic_bin_collision};
    use bpp_core::{AssetId, InstanceId, ResolvedPhysicalProperties, RunConfig};

    use super::{
        DynamicBodySpec, PhysicsWorld, rigid_transform_from_isometry, rigid_transform_to_isometry,
    };

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

    #[test]
    fn dynamic_body_mass_is_independent_of_part_count_and_colliders_use_contact_properties() {
        let asset_id = AssetId::new("part").expect("nonempty asset ID");
        let instance_id = InstanceId::new(1).expect("nonzero instance ID");
        let properties = ResolvedPhysicalProperties::try_new(2.5, 0.73, 0.27)
            .expect("valid physical properties");
        let part = ConvexPart::try_new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3],
        )
        .expect("tetrahedron is checked");
        let one_part = vec![part.clone()];
        let two_parts = vec![part.clone(), part];
        let mut one_part_world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");
        let mut two_part_world =
            PhysicsWorld::try_new(&bin([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 0.5, 0.1))
                .expect("finite bin constructs");

        one_part_world
            .insert_dynamic_bodies(&[DynamicBodySpec::new(
                instance_id,
                &asset_id,
                &one_part,
                bpp_core::RigidTransform::identity(),
                properties,
            )])
            .expect("one-part body inserts");
        two_part_world
            .insert_dynamic_bodies(&[DynamicBodySpec::new(
                instance_id,
                &asset_id,
                &two_parts,
                bpp_core::RigidTransform::identity(),
                properties,
            )])
            .expect("two-part body inserts");

        for world in [&one_part_world, &two_part_world] {
            let metadata = world
                .dynamic_bodies
                .get(&instance_id)
                .expect("inserted body metadata is stored");
            assert_eq!(
                world.rigid_bodies[metadata.body_handle].mass().to_bits(),
                properties.mass_kg().to_bits()
            );
            for &collider_handle in &metadata.collider_handles {
                let collider = &world.colliders[collider_handle];
                assert_eq!(
                    collider.friction().to_bits(),
                    properties.friction().to_bits()
                );
                assert_eq!(
                    collider.restitution().to_bits(),
                    properties.restitution().to_bits()
                );
            }
        }
    }
}

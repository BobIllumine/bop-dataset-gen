//! Camera, shadow, and color math, and vertex packing.

#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the crate root's imports and helpers"
)]
use super::*;

pub(crate) fn camera_uniform(
    camera: &CameraSnapshot,
    appearance: &AppearanceSnapshot,
    camera_from_world: glam::Mat4,
    light_clip_from_world: glam::Mat4,
    shadow_enabled: bool,
    config: WgpuRendererConfig,
    environment: &crate::environment::EnvironmentUniform,
) -> Result<CameraUniform, WgpuSceneError> {
    if appearance.directional_lights().len() > MAX_LIGHTS
        || appearance.point_lights().len() > MAX_LIGHTS
    {
        return Err(WgpuSceneError::TooManyLights {
            maximum: MAX_LIGHTS,
        });
    }
    let size = camera.image_size();
    let intrinsics = camera.intrinsics();
    let mut directional_directions = [[0.0; 4]; MAX_LIGHTS];
    let mut directional_colors = [[0.0; 4]; MAX_LIGHTS];
    for (index, light) in appearance.directional_lights().iter().copied().enumerate() {
        let direction = light.direction_world();
        directional_directions[index] = [direction[0], direction[1], direction[2], 0.0];
        let color = light.color().rgb();
        let scale = light.illuminance_lux().get() / 1000.0;
        directional_colors[index] = [color[0] * scale, color[1] * scale, color[2] * scale, 0.0];
    }
    let mut point_positions = [[0.0; 4]; MAX_LIGHTS];
    let mut point_colors = [[0.0; 4]; MAX_LIGHTS];
    for (index, light) in appearance.point_lights().iter().copied().enumerate() {
        let position = light.position_world_m().map(bop_core::Meters::get);
        point_positions[index] = [position[0], position[1], position[2], 0.0];
        let color = light.color().rgb();
        let scale = light.luminous_power_lm().get() / (4.0 * std::f32::consts::PI);
        point_colors[index] = [color[0] * scale, color[1] * scale, color[2] * scale, 0.0];
    }
    let ambient = appearance.ambient_light();
    let ambient_color = ambient.color().rgb();
    let ambient_scale = ambient.intensity().get();
    let camera_position = camera_from_world
        .inverse()
        .transform_point3(glam::Vec3::ZERO)
        .to_array();
    Ok(CameraUniform {
        camera_from_world: camera_from_world.to_cols_array_2d(),
        projection: [
            intrinsics.fx(),
            intrinsics.skew(),
            intrinsics.cx(),
            intrinsics.fy(),
        ],
        image_and_clip: [
            intrinsics.cy(),
            dimension_as_f32(size.width())?,
            dimension_as_f32(size.height())?,
            camera.near_m().get(),
        ],
        far_and_padding: [camera.far_m().get(), 0.0, 0.0, 0.0],
        camera_world_position: [
            camera_position[0],
            camera_position[1],
            camera_position[2],
            0.0,
        ],
        ambient: [
            ambient_color[0] * ambient_scale,
            ambient_color[1] * ambient_scale,
            ambient_color[2] * ambient_scale,
            0.0,
        ],
        light_counts: [
            u32::try_from(appearance.directional_lights().len()).expect("bounded"),
            u32::try_from(appearance.point_lights().len()).expect("bounded"),
            0,
            0,
        ],
        directional_directions,
        directional_colors,
        point_positions,
        point_colors,
        light_clip_from_world: light_clip_from_world.to_cols_array_2d(),
        // Enabled, depth bias (NDC), normal offset (m), filter spread (texels).
        shadow: [f32::from(shadow_enabled), 0.000_2, 0.002, 1.0],
        // AO radius (m), strength exponent, GTAO slices, reflections on (0/1).
        occlusion: [
            config.ambient_occlusion_radius_m(),
            1.0,
            4.0,
            f32::from(u8::from(config.screen_space_reflections())),
        ],
        environment: environment.environment,
        environment_levels: environment.levels,
        environment_sh: environment.sh,
    })
}

pub(crate) fn primitive_bounds(primitive: &VisualPrimitive) -> ([f32; 3], [f32; 3]) {
    let mut minimum = glam::Vec3::splat(f32::INFINITY);
    let mut maximum = glam::Vec3::splat(f32::NEG_INFINITY);
    for position in primitive.positions() {
        let point = glam::Vec3::from_array(*position);
        minimum = minimum.min(point);
        maximum = maximum.max(point);
    }
    (minimum.to_array(), maximum.to_array())
}

pub(crate) fn shadow_projection(
    appearance: &AppearanceSnapshot,
    uniforms: &[DrawUniform],
    calls: &[DrawCall<'_>],
) -> (glam::Mat4, bool) {
    let Some(light) = appearance.directional_lights().first() else {
        return (glam::Mat4::IDENTITY, false);
    };
    let mut minimum = glam::Vec3::splat(f32::INFINITY);
    let mut maximum = glam::Vec3::splat(f32::NEG_INFINITY);
    for (uniform, call) in uniforms.iter().zip(calls) {
        let world_from_model = glam::Mat4::from_cols_array_2d(&uniform.world_from_model);
        for x in [call.primitive.bounds_min[0], call.primitive.bounds_max[0]] {
            for y in [call.primitive.bounds_min[1], call.primitive.bounds_max[1]] {
                for z in [call.primitive.bounds_min[2], call.primitive.bounds_max[2]] {
                    let point = world_from_model.transform_point3(glam::vec3(x, y, z));
                    minimum = minimum.min(point);
                    maximum = maximum.max(point);
                }
            }
        }
    }
    if !minimum.is_finite() || !maximum.is_finite() {
        return (glam::Mat4::IDENTITY, false);
    }
    let direction = glam::Vec3::from_array(light.direction_world()).normalize();
    (directional_shadow_matrix(direction, minimum, maximum), true)
}

pub(crate) fn directional_shadow_matrix(
    direction: glam::Vec3,
    minimum: glam::Vec3,
    maximum: glam::Vec3,
) -> glam::Mat4 {
    let center = (minimum + maximum) * 0.5;
    let radius = ((maximum - minimum).length() * 0.5).max(0.01);
    let up = if direction.z.abs() > 0.95 {
        glam::Vec3::X
    } else {
        glam::Vec3::Z
    };
    let eye = center - direction * (radius * 2.0 + 0.1);
    let view = glam::Mat4::look_at_rh(eye, center, up);
    let projection =
        glam::Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.01, radius * 4.0 + 0.2);
    projection * view
}

pub(crate) fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

pub(crate) fn rigid_matrix(transform: RigidTransform) -> glam::Mat4 {
    let translation = transform.translation().map(bop_core::Meters::get);
    glam::Mat4::from_rotation_translation(
        glam::Quat::from_array(transform.rotation_xyzw()),
        glam::Vec3::from_array(translation),
    )
}

pub(crate) fn rotation_matrix(transform: RigidTransform) -> glam::Mat4 {
    glam::Mat4::from_quat(glam::Quat::from_array(transform.rotation_xyzw()))
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn dimension_as_f32(value: u32) -> Result<f32, WgpuSceneError> {
    if value > 1 << 24 {
        return Err(WgpuSceneError::ImageDimensionPrecision { value });
    }
    Ok(value as f32)
}

/// Uploads the `[-1, 1]^3` cube that bin cuboids scale; faces wind
/// counter-clockwise seen from outside, matching the shadow pass's culling.
pub(crate) fn create_unit_cube(device: &wgpu::Device) -> GpuPrimitive {
    use wgpu::util::DeviceExt as _;
    // Each face: outward normal, then two in-plane axes whose cross product is the normal.
    const FACES: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ];
    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::<u32>::with_capacity(36);
    for (normal, u, v) in FACES {
        let [normal, u, v] = [normal, u, v].map(glam::Vec3::from_array);
        let base = u32::try_from(vertices.len()).expect("24 vertices fit in u32");
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            vertices.push(GpuVertex {
                position: (normal + u * su + v * sv).to_array(),
                normal: normal.to_array(),
                uv: [0.0, 0.0],
            });
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bop bin cube vertices"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bop bin cube indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    GpuPrimitive {
        vertex_buffer,
        index_buffer,
        index_count: 36,
        base_color: [1.0; 4],
        metallic: 0.0,
        roughness: 1.0,
        has_file_material: false,
        bounds_min: [-1.0; 3],
        bounds_max: [1.0; 3],
        material: None,
    }
}

/// Interleaves positions, normals and texture-coordinate set `uv_set`
/// (zero when the primitive has no such set).
pub(crate) fn pack_vertices(primitive: &VisualPrimitive, uv_set: usize) -> Vec<GpuVertex> {
    let texture_coordinates = primitive.tex_coord_sets().get(uv_set);
    primitive
        .positions()
        .iter()
        .copied()
        .zip(primitive.normals().iter().copied())
        .enumerate()
        .map(|(index, (position, normal))| GpuVertex {
            position,
            normal,
            uv: texture_coordinates.map_or([0.0, 0.0], |set| set[index]),
        })
        .collect()
}

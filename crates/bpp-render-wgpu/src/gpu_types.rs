//! GPU vertex and uniform layouts shared with the WGSL shaders.

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct GpuVertex {
    pub(crate) position: [f32; 3],
    pub(crate) normal: [f32; 3],
    pub(crate) uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CameraUniform {
    pub(crate) camera_from_world: [[f32; 4]; 4],
    pub(crate) projection: [f32; 4],
    pub(crate) image_and_clip: [f32; 4],
    pub(crate) far_and_padding: [f32; 4],
    pub(crate) camera_world_position: [f32; 4],
    pub(crate) ambient: [f32; 4],
    pub(crate) light_counts: [u32; 4],
    pub(crate) directional_directions: [[f32; 4]; MAX_LIGHTS],
    pub(crate) directional_colors: [[f32; 4]; MAX_LIGHTS],
    pub(crate) point_positions: [[f32; 4]; MAX_LIGHTS],
    pub(crate) point_colors: [[f32; 4]; MAX_LIGHTS],
    pub(crate) light_clip_from_world: [[f32; 4]; 4],
    pub(crate) shadow: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DrawUniform {
    pub(crate) world_from_model: [[f32; 4]; 4],
    pub(crate) normal_from_model: [[f32; 4]; 4],
    pub(crate) base_color: [f32; 4],
    pub(crate) material: [f32; 4],
    pub(crate) instance_id: u32,
    pub(crate) padding: [u32; 3],
}

pub(crate) const MAX_LIGHTS: usize = 8;
pub(crate) const GPU_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];

struct Camera {
    camera_from_world: mat4x4<f32>,
    projection: vec4<f32>,
    image_and_clip: vec4<f32>,
    far_and_padding: vec4<f32>,
    camera_world_position: vec4<f32>,
    ambient: vec4<f32>,
    light_counts: vec4<u32>,
    directional_directions: array<vec4<f32>, 8>,
    directional_colors: array<vec4<f32>, 8>,
    point_positions: array<vec4<f32>, 8>,
    point_colors: array<vec4<f32>, 8>,
    light_clip_from_world: mat4x4<f32>,
    shadow: vec4<f32>,
};

struct DrawData {
    world_from_model: mat4x4<f32>,
    normal_from_model: mat4x4<f32>,
    base_color: vec4<f32>,
    material: vec4<f32>,
    instance_id: u32,
    _padding0: u32,
    _padding1: u32,
    _padding2: u32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<storage, read> draws: array<DrawData>;

struct VertexInput {
    @location(0) position: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput, @builtin(instance_index) draw_index: u32) -> @builtin(position) vec4<f32> {
    let world = draws[draw_index].world_from_model * vec4<f32>(input.position, 1.0);
    return camera.light_clip_from_world * world;
}

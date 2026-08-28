struct Camera {
    camera_from_world: mat4x4<f32>,
    projection: vec4<f32>,
    image_and_clip: vec4<f32>,
    far_and_padding: vec4<f32>,
};

struct DrawData {
    world_from_model: mat4x4<f32>,
    normal_from_model: mat4x4<f32>,
    base_color: vec4<f32>,
    instance_id: u32,
    _padding0: u32,
    _padding1: u32,
    _padding2: u32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<storage, read> draws: array<DrawData>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) camera_depth: f32,
    @location(2) @interpolate(flat) base_color: vec4<f32>,
    @location(3) @interpolate(flat) instance_id: u32,
};

@vertex
fn vs_main(input: VertexInput, @builtin(instance_index) draw_index: u32) -> VertexOutput {
    let draw = draws[draw_index];
    let world = draw.world_from_model * vec4<f32>(input.position, 1.0);
    let camera_point = camera.camera_from_world * world;
    let fx = camera.projection.x;
    let skew = camera.projection.y;
    let cx = camera.projection.z;
    let fy = camera.projection.w;
    let cy = camera.image_and_clip.x;
    let width = camera.image_and_clip.y;
    let height = camera.image_and_clip.z;
    let near = camera.image_and_clip.w;
    let far_plane = camera.far_and_padding.x;
    var output: VertexOutput;
    output.clip_position = vec4<f32>(
        2.0 * (fx * camera_point.x + skew * camera_point.y) / width
            + (2.0 * cx / width - 1.0) * camera_point.z,
        -2.0 * fy * camera_point.y / height
            + (1.0 - 2.0 * cy / height) * camera_point.z,
        far_plane / (far_plane - near) * camera_point.z
            - far_plane * near / (far_plane - near),
        camera_point.z,
    );
    output.world_normal = normalize((draw.normal_from_model * vec4<f32>(input.normal, 0.0)).xyz);
    output.camera_depth = camera_point.z;
    output.base_color = draw.base_color;
    output.instance_id = draw.instance_id;
    return output;
}

struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @location(1) depth_m: f32,
    @location(2) instance_id: u32,
};

@fragment
fn fs_main(input: VertexOutput) -> FragmentOutput {
    let light_direction = normalize(vec3<f32>(0.35, -0.45, 0.82));
    let diffuse = max(dot(normalize(input.world_normal), light_direction), 0.0);
    let illumination = 0.2 + 0.8 * diffuse;
    var output: FragmentOutput;
    output.color = vec4<f32>(input.base_color.rgb * illumination, input.base_color.a);
    output.depth_m = max(input.camera_depth, 0.0);
    output.instance_id = input.instance_id;
    return output;
}

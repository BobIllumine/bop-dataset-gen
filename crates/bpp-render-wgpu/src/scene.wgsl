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
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) camera_depth: f32,
    @location(2) @interpolate(flat) base_color: vec4<f32>,
    @location(3) @interpolate(flat) instance_id: u32,
    @location(4) world_position: vec3<f32>,
    @location(5) @interpolate(flat) material: vec2<f32>,
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
    output.world_position = world.xyz;
    output.material = draw.material.xy;
    return output;
}

struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @location(1) depth_m: f32,
    @location(2) instance_id: u32,
};

@fragment
fn fs_main(input: VertexOutput) -> FragmentOutput {
    let normal = normalize(input.world_normal);
    let view_direction = normalize(camera.camera_world_position.xyz - input.world_position);
    let metallic = input.material.x;
    let roughness = max(input.material.y, 0.04);
    var illumination = camera.ambient.rgb;
    var specular = vec3<f32>(0.0);
    for (var index = 0u; index < camera.light_counts.x; index++) {
        let light_direction = normalize(-camera.directional_directions[index].xyz);
        let radiance = camera.directional_colors[index].rgb;
        let diffuse = max(dot(normal, light_direction), 0.0);
        let half_direction = normalize(light_direction + view_direction);
        let shininess = max(2.0, 2.0 / (roughness * roughness) - 2.0);
        let highlight = pow(max(dot(normal, half_direction), 0.0), shininess);
        illumination += radiance * diffuse;
        specular += radiance * highlight;
    }
    for (var index = 0u; index < camera.light_counts.y; index++) {
        let offset = camera.point_positions[index].xyz - input.world_position;
        let distance_squared = max(dot(offset, offset), 0.0001);
        let light_direction = normalize(offset);
        let radiance = camera.point_colors[index].rgb / distance_squared;
        let diffuse = max(dot(normal, light_direction), 0.0);
        let half_direction = normalize(light_direction + view_direction);
        let shininess = max(2.0, 2.0 / (roughness * roughness) - 2.0);
        let highlight = pow(max(dot(normal, half_direction), 0.0), shininess);
        illumination += radiance * diffuse;
        specular += radiance * highlight;
    }
    let dielectric_specular = vec3<f32>(0.04);
    let specular_color = mix(dielectric_specular, input.base_color.rgb, metallic);
    let diffuse_color = input.base_color.rgb * (1.0 - metallic);
    var output: FragmentOutput;
    output.color = vec4<f32>(diffuse_color * illumination + specular_color * specular, input.base_color.a);
    output.depth_m = max(input.camera_depth, 0.0);
    output.instance_id = input.instance_id;
    return output;
}

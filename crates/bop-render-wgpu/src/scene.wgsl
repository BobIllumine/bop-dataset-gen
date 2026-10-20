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
    occlusion: vec4<f32>,
    environment: vec4<f32>,
    environment_levels: vec4<f32>,
    environment_sh: array<vec4<f32>, 9>,
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
@group(2) @binding(0) var shadow_map: texture_depth_2d;
@group(2) @binding(1) var shadow_sampler: sampler_comparison;
@group(2) @binding(2) var occlusion_map: texture_2d<f32>;
@group(3) @binding(0) var base_color_texture: texture_2d<f32>;
@group(3) @binding(1) var base_color_sampler: sampler;

const PI: f32 = 3.14159265;

/// Fraction of the first directional light reaching `world_position`:
/// a 5x5 percentage-closer filter over the hardware-filtered comparison,
/// sampled from a point pushed out along the normal so surfaces do not
/// shadow themselves.
fn directional_shadow(world_position: vec3<f32>, normal: vec3<f32>) -> f32 {
    if camera.shadow.x < 0.5 {
        return 1.0;
    }
    let light_direction = normalize(-camera.directional_directions[0].xyz);
    let grazing = 1.0 - max(dot(normal, light_direction), 0.0);
    let offset = normal * camera.shadow.z * (0.5 + grazing);
    let clip = camera.light_clip_from_world * vec4<f32>(world_position + offset, 1.0);
    let ndc = clip.xyz / clip.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let texel = 1.0 / f32(textureDimensions(shadow_map).x);
    let reference = ndc.z - camera.shadow.y;
    var lit = 0.0;
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            let tap = uv + vec2<f32>(f32(x), f32(y)) * texel * camera.shadow.w;
            lit += textureSampleCompareLevel(shadow_map, shadow_sampler, tap, reference);
        }
    }
    return lit / 25.0;
}

/// Ambient occlusion corrected for light bouncing between nearby surfaces of
/// the given albedo (Jimenez et al. 2016, fitted to ray-traced references).
fn multi_bounce(visibility: f32, albedo: vec3<f32>) -> vec3<f32> {
    let a = 2.0404 * albedo - 0.3324;
    let b = -4.7951 * albedo + 0.6417;
    let c = 2.7552 * albedo + 0.6903;
    let v = vec3<f32>(visibility);
    return max(v, ((v * a + b) * v + c) * v);
}

/// GGX normal distribution.
fn distribution_ggx(n_dot_h: f32, alpha: f32) -> f32 {
    let alpha2 = alpha * alpha;
    let denominator = n_dot_h * n_dot_h * (alpha2 - 1.0) + 1.0;
    return alpha2 / (PI * denominator * denominator);
}

/// Height-correlated Smith visibility, G / (4 n.l n.v).
fn visibility_smith(n_dot_l: f32, n_dot_v: f32, alpha: f32) -> f32 {
    let alpha2 = alpha * alpha;
    let lambda_v = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - alpha2) + alpha2);
    let lambda_l = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - alpha2) + alpha2);
    return 0.5 / max(lambda_v + lambda_l, 1e-5);
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - cos_theta, 5.0);
}

/// Hemispherical-directional reflectance of the specular lobe (Karis's
/// analytic fit to the split-sum table), for light from a uniform environment.
fn environment_brdf(f0: vec3<f32>, roughness: f32, n_dot_v: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * n_dot_v)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + ab.y;
}

/// The environment's direction for world direction `d`: the map is turned
/// by its rotation about +Z, so sampling turns the other way.
fn environment_direction(d: vec3<f32>) -> vec3<f32> {
    let c = camera.environment.z;
    let s = camera.environment.w;
    return vec3<f32>(c * d.x + s * d.y, -s * d.x + c * d.y, d.z);
}

/// Radiance a white diffuse surface with normal `n` reflects.
fn environment_diffuse(n: vec3<f32>) -> vec3<f32> {
    let d = environment_direction(n);
    let sh = camera.environment_sh;
    let basis = array<f32, 9>(
        0.282095,
        0.488603 * d.y,
        0.488603 * d.z,
        0.488603 * d.x,
        1.092548 * d.x * d.y,
        1.092548 * d.y * d.z,
        0.315392 * (3.0 * d.z * d.z - 1.0),
        1.092548 * d.x * d.z,
        0.546274 * (d.x * d.x - d.y * d.y),
    );
    var irradiance = vec3<f32>(0.0);
    for (var i = 0u; i < 9u; i++) {
        irradiance += sh[i].rgb * basis[i];
    }
    return max(irradiance, vec3<f32>(0.0));
}

/// Light one surface point receives from one light of the given radiance
/// (scaled so a white diffuse surface facing it reflects `radiance`), split
/// into the diffuse and specular contributions.
fn shade_light(
    normal: vec3<f32>,
    view_direction: vec3<f32>,
    light_direction: vec3<f32>,
    radiance: vec3<f32>,
    diffuse_color: vec3<f32>,
    f0: vec3<f32>,
    alpha: f32,
) -> vec3<f32> {
    let n_dot_l = dot(normal, light_direction);
    if n_dot_l <= 0.0 {
        return vec3<f32>(0.0);
    }
    let n_dot_v = max(dot(normal, view_direction), 1e-4);
    let half_direction = normalize(light_direction + view_direction);
    let n_dot_h = max(dot(normal, half_direction), 0.0);
    let fresnel = fresnel_schlick(max(dot(view_direction, half_direction), 0.0), f0);
    // Irradiance is pi * radiance in these units; the diffuse lobe is albedo / pi.
    let specular = PI * distribution_ggx(n_dot_h, alpha) * visibility_smith(n_dot_l, n_dot_v, alpha) * fresnel;
    let diffuse = diffuse_color * (vec3<f32>(1.0) - fresnel);
    return (diffuse + specular) * radiance * n_dot_l;
}

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
    @location(6) uv: vec2<f32>,
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
    output.uv = input.uv;
    return output;
}

struct PrepassOutput {
    @location(0) world_normal: vec4<f32>,
    @location(1) depth_m: f32,
};

/// Depth, camera Z and world normals for ambient occlusion, before shading.
@fragment
fn fs_prepass(input: VertexOutput) -> PrepassOutput {
    var normal = normalize(input.world_normal);
    if dot(normal, camera.camera_world_position.xyz - input.world_position) < 0.0 {
        normal = -normal;
    }
    var output: PrepassOutput;
    output.world_normal = vec4<f32>(normal, 0.0);
    output.depth_m = max(input.camera_depth, 0.0);
    return output;
}

struct FragmentOutput {
    // Everything but the specular environment term, which the reflection
    // pass adds from the screen or the environment.
    @location(0) color: vec4<f32>,
    // That term's weight (Fresnel and environment intensity) and the roughness.
    @location(1) reflectance: vec4<f32>,
    @location(2) depth_m: f32,
    @location(3) instance_id: u32,
};

@fragment
fn fs_main(input: VertexOutput) -> FragmentOutput {
    var normal = normalize(input.world_normal);
    let view_direction = normalize(camera.camera_world_position.xyz - input.world_position);
    // Back faces seen through gaps shade as their front.
    if dot(normal, view_direction) < 0.0 {
        normal = -normal;
    }
    let metallic = input.material.x;
    let roughness = clamp(input.material.y, 0.03, 1.0);
    let alpha = roughness * roughness;
    // glTF: the base-color factor times the texture (white when untextured).
    let base_color = input.base_color.rgb * textureSample(base_color_texture, base_color_sampler, input.uv).rgb;
    let f0 = mix(vec3<f32>(0.04), base_color, metallic);
    let diffuse_color = base_color * (1.0 - metallic);
    let n_dot_v = max(dot(normal, view_direction), 1e-4);

    // The uniform environment: diffuse irradiance plus its specular
    // reflection, both reduced where nearby geometry hides it.
    let occlusion = textureLoad(occlusion_map, vec2<i32>(input.clip_position.xy), 0).r;
    let reflectance = environment_brdf(f0, roughness, n_dot_v);
    var color: vec3<f32>;
    var specular_weight: vec3<f32>;
    if camera.environment.x > 0.5 {
        let intensity = camera.environment.y;
        color = multi_bounce(occlusion, diffuse_color) * intensity * diffuse_color * (vec3<f32>(1.0) - reflectance) * environment_diffuse(normal);
        specular_weight = intensity * reflectance;
    } else {
        color = multi_bounce(occlusion, diffuse_color) * camera.ambient.rgb * diffuse_color * (vec3<f32>(1.0) - reflectance);
        specular_weight = reflectance;
    }
    for (var index = 0u; index < camera.light_counts.x; index++) {
        let light_direction = normalize(-camera.directional_directions[index].xyz);
        let visibility = select(1.0, directional_shadow(input.world_position, normal), index == 0u);
        color += visibility * shade_light(normal, view_direction, light_direction, camera.directional_colors[index].rgb, diffuse_color, f0, alpha);
    }
    for (var index = 0u; index < camera.light_counts.y; index++) {
        let offset = camera.point_positions[index].xyz - input.world_position;
        let distance_squared = max(dot(offset, offset), 0.0001);
        let radiance = camera.point_colors[index].rgb / distance_squared;
        color += shade_light(normal, view_direction, normalize(offset), radiance, diffuse_color, f0, alpha);
    }
    var output: FragmentOutput;
    output.color = vec4<f32>(color, input.base_color.a);
    output.reflectance = vec4<f32>(specular_weight, roughness);
    output.depth_m = max(input.camera_depth, 0.0);
    output.instance_id = input.instance_id;
    return output;
}

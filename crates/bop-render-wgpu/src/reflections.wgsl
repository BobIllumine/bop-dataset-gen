// Screen-space reflections and the final composite. The main pass leaves the
// scene without its specular environment term in `base` (with a mip chain)
// and, per pixel, that term's weight with the roughness in `reflectance`.
// Each pixel marches its dominant reflection direction through the prepass
// depth. A hit reflects what the camera sees there (a bin wall, another
// part), read from the mip level matching the GGX lobe's footprint, so rough
// surfaces reflect it blurred. A miss reflects the environment, dimmed by
// ambient occlusion.

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
    // Radius, strength, slices, screen-space reflections enabled (0/1).
    occlusion: vec4<f32>,
    environment: vec4<f32>,
    environment_levels: vec4<f32>,
    environment_sh: array<vec4<f32>, 9>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var base: texture_2d<f32>;
@group(0) @binding(2) var reflectance: texture_2d<f32>;
@group(0) @binding(3) var normals: texture_2d<f32>;
@group(0) @binding(4) var depths: texture_2d<f32>;
@group(0) @binding(5) var environment_map: texture_2d<f32>;
@group(0) @binding(6) var environment_sampler: sampler;
@group(0) @binding(7) var screen_sampler: sampler;
@group(0) @binding(8) var occlusion_map: texture_2d<f32>;

const PI: f32 = 3.14159265;
const STEPS: u32 = 64u;
const REFINEMENTS: u32 = 6u;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var output: VertexOutput;
    output.position = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    return output;
}

fn unproject(pixel: vec2<f32>, z: f32) -> vec3<f32> {
    let y = (pixel.y - camera.image_and_clip.x) * z / camera.projection.w;
    let x = (pixel.x - camera.projection.z - camera.projection.y * y / z) * z / camera.projection.x;
    return vec3<f32>(x, y, z);
}

fn project(point: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(
        (camera.projection.x * point.x + camera.projection.y * point.y) / point.z + camera.projection.z,
        camera.projection.w * point.y / point.z + camera.image_and_clip.x,
    );
}

fn environment_uv(d: vec3<f32>) -> vec2<f32> {
    let c = camera.environment.z;
    let s = camera.environment.w;
    let n = normalize(vec3<f32>(c * d.x + s * d.y, -s * d.x + c * d.y, d.z));
    return vec2<f32>(0.5 - atan2(n.y, n.x) / (2.0 * PI), acos(clamp(n.z, -1.0, 1.0)) / PI);
}

/// Radiance reflected from beyond the screen: the environment, or the
/// uniform ambient light.
fn fallback(world_direction: vec3<f32>, roughness: f32) -> vec3<f32> {
    if camera.environment.x > 0.5 {
        let level = roughness * camera.environment_levels.x;
        return textureSampleLevel(environment_map, environment_sampler, environment_uv(world_direction), level).rgb;
    }
    return camera.ambient.rgb;
}

/// Marches `direction` from `origin` (camera frame, metres) through the depth
/// buffer. Returns the hit pixel and a confidence (0 for a miss).
fn trace(origin: vec3<f32>, direction: vec3<f32>, size: vec2<f32>) -> vec3<f32> {
    // Clip the ray in front of the near plane and to a few metres.
    let near = camera.image_and_clip.w;
    var length = 1.5;
    if direction.z < 0.0 {
        length = min(length, (near - origin.z) / direction.z * 0.99);
    }
    let end = origin + direction * length;
    let start_pixel = project(origin);
    let end_pixel = project(end);
    let pixels = max(abs(end_pixel.x - start_pixel.x), abs(end_pixel.y - start_pixel.y));
    if pixels < 1.0 {
        return vec3<f32>(0.0);
    }
    // Perspective-correct: interpolate 1/z along the screen-space line.
    let inv_start = 1.0 / origin.z;
    let inv_end = 1.0 / end.z;
    var previous = 0.0;
    for (var i = 1u; i <= STEPS; i++) {
        // Denser near the origin, where contact reflections matter most.
        let t = pow(f32(i) / f32(STEPS), 1.6);
        let pixel = mix(start_pixel, end_pixel, t);
        if any(pixel < vec2<f32>(0.0)) || any(pixel >= size) {
            return vec3<f32>(0.0);
        }
        let ray_z = 1.0 / mix(inv_start, inv_end, t);
        let surface_z = textureLoad(depths, vec2<i32>(pixel), 0).r;
        if surface_z > 0.0 && ray_z > surface_z {
            // Refine between the last miss and this sample.
            var low = previous;
            var high = t;
            for (var r = 0u; r < REFINEMENTS; r++) {
                let mid = 0.5 * (low + high);
                let mid_pixel = mix(start_pixel, end_pixel, mid);
                let mid_z = 1.0 / mix(inv_start, inv_end, mid);
                let mid_surface = textureLoad(depths, vec2<i32>(mid_pixel), 0).r;
                if mid_surface > 0.0 && mid_z > mid_surface {
                    high = mid;
                } else {
                    low = mid;
                }
            }
            let hit_pixel = mix(start_pixel, end_pixel, high);
            let hit_ray_z = 1.0 / mix(inv_start, inv_end, high);
            let hit_surface = textureLoad(depths, vec2<i32>(hit_pixel), 0).r;
            // Passing behind a thin object is not a hit.
            let thickness = 0.03 + 0.02 * hit_surface;
            if hit_surface <= 0.0 || hit_ray_z - hit_surface > thickness {
                return vec3<f32>(0.0);
            }
            // Fade near the image border, where the screen runs out.
            let border = min(min(hit_pixel.x, size.x - hit_pixel.x), min(hit_pixel.y, size.y - hit_pixel.y));
            let confidence = smoothstep(0.0, 0.08 * min(size.x, size.y), border);
            return vec3<f32>(hit_pixel, confidence);
        }
        previous = t;
    }
    return vec3<f32>(0.0);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(input.position.xy);
    let lit = textureLoad(base, texel, 0);
    let surface = textureLoad(reflectance, texel, 0);
    let weight = surface.rgb;
    if max(weight.r, max(weight.g, weight.b)) <= 0.0 {
        return lit;
    }
    let roughness = surface.a;
    let z = textureLoad(depths, texel, 0).r;
    let rotation = mat3x3<f32>(
        camera.camera_from_world[0].xyz,
        camera.camera_from_world[1].xyz,
        camera.camera_from_world[2].xyz,
    );
    let normal = normalize(rotation * textureLoad(normals, texel, 0).xyz);
    let origin = unproject(input.position.xy, z);
    let mirror = reflect(normalize(origin), normal);
    // The GGX lobe's dominant direction leans toward the normal as it widens
    // (Lagarde & de Rousiers 2014).
    let lean = (1.0 - roughness) * (sqrt(1.0 - roughness) + roughness);
    let reflected = normalize(mix(normal, mirror, lean));
    // Camera-from-world is a rotation, so its transpose maps back to world.
    let world_reflected = normalize(transpose(rotation) * reflected);
    let occlusion = textureLoad(occlusion_map, texel, 0).r;
    var radiance = occlusion * fallback(world_reflected, roughness);
    // One ray stands in for the whole GGX lobe only while the lobe is narrow;
    // rougher surfaces keep the environment and multi-bounce occlusion.
    if camera.occlusion.w > 0.5 && roughness < 0.6 {
        let size = vec2<f32>(camera.image_and_clip.y, camera.image_and_clip.z);
        // Start just off the surface so it does not hit itself.
        let start = origin + normal * (0.002 * z);
        let hit = trace(start, reflected, size);
        if hit.z > 0.0 {
            // Cone footprint at the hit, in pixels: GGX half-angle ~ atan(alpha).
            let hit_z = textureLoad(depths, vec2<i32>(hit.xy), 0).r;
            let distance = length(unproject(hit.xy, hit_z) - start);
            let footprint = 2.0 * distance * roughness * roughness * camera.projection.x / hit_z;
            let lod = log2(max(footprint, 1.0));
            let seen = textureSampleLevel(base, screen_sampler, hit.xy / size, lod).rgb;
            let confidence = hit.z * (1.0 - smoothstep(0.35, 0.6, roughness));
            radiance = mix(radiance, seen, confidence);
        }
    }
    return vec4<f32>(lit.rgb + weight * radiance, lit.a);
}

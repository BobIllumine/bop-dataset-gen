// Screen-space ambient occlusion from the prepass's camera-Z depth and world
// normals. The radius is in metres: depth is metric and the intrinsics are
// exact, so occlusion does not depend on resolution or distance.

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
    // Radius (m), strength exponent, sample count, unused.
    occlusion: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var normals: texture_2d<f32>;
@group(0) @binding(2) var depths: texture_2d<f32>;

const PI: f32 = 3.14159265;
const STEPS: u32 = 16u;

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

/// Camera-frame point (OpenCV axes) seen at pixel centre `pixel` at camera Z `z`.
fn unproject(pixel: vec2<f32>, z: f32) -> vec3<f32> {
    let fx = camera.projection.x;
    let skew = camera.projection.y;
    let cx = camera.projection.z;
    let fy = camera.projection.w;
    let cy = camera.image_and_clip.x;
    let y = (pixel.y - cy) * z / fy;
    let x = (pixel.x - cx - skew * y / z) * z / fx;
    return vec3<f32>(x, y, z);
}

fn project(point: vec3<f32>) -> vec2<f32> {
    let fx = camera.projection.x;
    let skew = camera.projection.y;
    let cx = camera.projection.z;
    let fy = camera.projection.w;
    let cy = camera.image_and_clip.x;
    return vec2<f32>(
        (fx * point.x + skew * point.y) / point.z + cx,
        fy * point.y / point.z + cy,
    );
}

/// A 4x4 ordered (Bayer) rotation per pixel: deterministic, and the 4x4 blur
/// below averages all sixteen offsets, so no pattern survives on smooth surfaces.
fn pixel_noise(pixel: vec2<f32>) -> f32 {
    let bayer = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
    let cell = vec2<u32>(pixel) % vec2<u32>(4u);
    return (bayer[cell.y * 4u + cell.x] + 0.5) / 16.0;
}

/// Ground-truth-based ambient occlusion (Jimenez et al. 2016, as in XeGTAO):
/// in each screen-space slice through the pixel, find the highest occluding
/// horizon on both sides within the radius and integrate cosine-weighted
/// visibility between them. With depth as a height field this measures how
/// much of the sky the surrounding geometry hides, not just nearby clutter.
@fragment
fn fs_occlusion(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(input.position.xy);
    let z = textureLoad(depths, texel, 0).r;
    let radius = camera.occlusion.x;
    if z <= 0.0 || radius <= 0.0 {
        return vec4<f32>(1.0);
    }
    let rotation = mat3x3<f32>(
        camera.camera_from_world[0].xyz,
        camera.camera_from_world[1].xyz,
        camera.camera_from_world[2].xyz,
    );
    let normal = normalize(rotation * textureLoad(normals, texel, 0).xyz);
    let origin = unproject(input.position.xy, z);
    let view = normalize(-origin);
    let size = vec2<f32>(camera.image_and_clip.y, camera.image_and_clip.z);
    let screen_radius = radius * camera.projection.x / z;
    let slices = max(u32(camera.occlusion.z), 1u);
    let noise = pixel_noise(input.position.xy);
    var visibility = 0.0;
    for (var slice = 0u; slice < slices; slice++) {
        let phi = (f32(slice) + noise) * PI / f32(slices);
        let omega = vec2<f32>(cos(phi), sin(phi));
        // OpenCV camera axes run along the image axes, so the slice direction
        // in camera space is omega itself.
        let direction = vec3<f32>(omega, 0.0);
        let ortho = direction - dot(direction, view) * view;
        let axis = normalize(cross(ortho, view));
        let projected = normal - axis * dot(normal, axis);
        let projected_length = length(projected);
        if projected_length < 1e-4 {
            continue;
        }
        let cos_n = clamp(dot(projected, view) / projected_length, -1.0, 1.0);
        let n = sign(dot(ortho, projected)) * acos(cos_n);
        var horizon_cos = vec2<f32>(-1.0);
        for (var step = 0u; step < STEPS; step++) {
            // Quadratic spacing, jittered: dense near the pixel, reaching the radius.
            let t = (f32(step) + fract(noise * 7.13 + f32(step) * 0.618)) / f32(STEPS);
            let offset = omega * max(t * t * screen_radius, 1.0 + f32(step));
            for (var side = 0; side < 2; side++) {
                let pixel = input.position.xy + select(offset, -offset, side == 1);
                if any(pixel < vec2<f32>(0.0)) || any(pixel >= size) {
                    continue;
                }
                let sample_z = textureLoad(depths, vec2<i32>(pixel), 0).r;
                if sample_z <= 0.0 {
                    continue;
                }
                let delta = unproject(pixel, sample_z) - origin;
                let distance = length(delta);
                let weight = clamp(2.0 - 2.0 * distance / radius, 0.0, 1.0);
                let cos_h = mix(-1.0, dot(delta / distance, view), weight);
                horizon_cos[side] = max(horizon_cos[side], cos_h);
            }
        }
        let h0 = n + clamp(acos(horizon_cos.x) - n, -PI / 2.0, PI / 2.0);
        let h1 = n + clamp(-acos(horizon_cos.y) - n, -PI / 2.0, PI / 2.0);
        let arc0 = (cos_n + 2.0 * h0 * sin(n) - cos(2.0 * h0 - n)) / 4.0;
        let arc1 = (cos_n + 2.0 * h1 * sin(n) - cos(2.0 * h1 - n)) / 4.0;
        visibility += projected_length * (arc0 + arc1);
    }
    visibility = clamp(visibility / f32(slices), 0.0, 1.0);
    return vec4<f32>(pow(visibility, camera.occlusion.y));
}

@group(0) @binding(3) var raw_occlusion: texture_2d<f32>;

/// 4x4 depth-aware box blur, matching the noise's period.
@fragment
fn fs_blur(input: VertexOutput) -> @location(0) vec4<f32> {
    let centre = vec2<i32>(input.position.xy);
    let z = textureLoad(depths, centre, 0).r;
    if z <= 0.0 {
        return vec4<f32>(1.0);
    }
    let limit = vec2<i32>(textureDimensions(depths)) - 1;
    var total = 0.0;
    var weight = 0.0;
    for (var y = -2; y < 2; y++) {
        for (var x = -2; x < 2; x++) {
            let tap = clamp(centre + vec2<i32>(x, y), vec2<i32>(0), limit);
            let tap_z = textureLoad(depths, tap, 0).r;
            let w = select(0.0, exp(-abs(tap_z - z) / (0.01 * z)), tap_z > 0.0);
            total += textureLoad(raw_occlusion, tap, 0).r * w;
            weight += w;
        }
    }
    return vec4<f32>(total / max(weight, 1e-6));
}

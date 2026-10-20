@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> image_size: vec4<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let position = positions[index];
    var output: VertexOutput;
    output.position = vec4<f32>(position, 0.0, 1.0);
    output.uv = vec2<f32>(position.x * 0.5 + 0.5, 0.5 - position.y * 0.5);
    return output;
}

fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.299, 0.587, 0.114));
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = 1.0 / image_size.xy;
    let center = textureSample(source, source_sampler, input.uv);
    let north_west = textureSample(source, source_sampler, input.uv + vec2<f32>(-texel.x, -texel.y));
    let north_east = textureSample(source, source_sampler, input.uv + vec2<f32>(texel.x, -texel.y));
    let south_west = textureSample(source, source_sampler, input.uv + vec2<f32>(-texel.x, texel.y));
    let south_east = textureSample(source, source_sampler, input.uv + texel);
    let minimum = min(luminance(center.rgb), min(min(luminance(north_west.rgb), luminance(north_east.rgb)), min(luminance(south_west.rgb), luminance(south_east.rgb))));
    let maximum = max(luminance(center.rgb), max(max(luminance(north_west.rgb), luminance(north_east.rgb)), max(luminance(south_west.rgb), luminance(south_east.rgb))));
    if maximum - minimum < max(0.0312, maximum * 0.125) {
        return center;
    }
    var direction = vec2<f32>(
        -(luminance(north_west.rgb) + luminance(north_east.rgb) - luminance(south_west.rgb) - luminance(south_east.rgb)),
        luminance(north_west.rgb) + luminance(south_west.rgb) - luminance(north_east.rgb) - luminance(south_east.rgb),
    );
    let reduction = max((luminance(north_west.rgb) + luminance(north_east.rgb) + luminance(south_west.rgb) + luminance(south_east.rgb)) * 0.03125, 0.0078125);
    direction = clamp(direction / (min(abs(direction.x), abs(direction.y)) + reduction), vec2<f32>(-8.0), vec2<f32>(8.0)) * texel;
    let first = 0.5 * (
        textureSample(source, source_sampler, input.uv + direction * (-1.0 / 6.0))
        + textureSample(source, source_sampler, input.uv + direction * (1.0 / 6.0))
    );
    let second = first * 0.5 + 0.25 * (
        textureSample(source, source_sampler, input.uv + direction * -0.5)
        + textureSample(source, source_sampler, input.uv + direction * 0.5)
    );
    let second_luminance = luminance(second.rgb);
    return select(second, first, second_luminance < minimum || second_luminance > maximum);
}

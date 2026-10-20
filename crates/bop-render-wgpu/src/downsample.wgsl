// One level of the lit scene's mip chain: a 2x2 box filter of the level above.

@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let limit = vec2<i32>(textureDimensions(source)) - 1;
    let origin = vec2<i32>(position.xy) * 2;
    var sum = vec4<f32>(0.0);
    for (var y = 0; y < 2; y++) {
        for (var x = 0; x < 2; x++) {
            sum += textureLoad(source, min(origin + vec2<i32>(x, y), limit), 0);
        }
    }
    return sum / 4.0;
}

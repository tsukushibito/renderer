@vertex
fn vertices(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

@group(0) @binding(0) var frame: texture_2d<f32>;

@fragment
fn display(@builtin(position) pixel: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(textureLoad(frame, vec2<i32>(pixel.xy), 0).rgb, 1.0);
}


// Caller-supplied compute module: sampled raster color/depth -> storage output.
// All 8x4 texels are written by the exact dispatch below; no implicit zero-init
// is needed for output validity in this fixture.
@group(0) @binding(0) var color: texture_2d<f32>;
@group(0) @binding(1) var depth: texture_depth_2d;
@group(0) @binding(2) var output: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(4, 4)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel = vec2<i32>(id.xy);
    let source = textureLoad(color, pixel, 0);
    let z = textureLoad(depth, pixel, 0);
    // Keep rendered depth 0.25 distinguishable from the clear value 1.0.
    textureStore(output, pixel, vec4(source.b, source.r, z, 1.0));
}

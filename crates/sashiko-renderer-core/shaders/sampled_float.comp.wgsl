struct type_8 {
    member: array<vec4<u32>, 4>,
}

@group(0) @binding(0)
var global: texture_2d<f32>;
@group(0) @binding(1)
var global_1: sampler;
@group(1) @binding(0)
var<storage, read_write> global_2: type_8;

fn function_() {
    let _e19 = textureSampleLevel(global, global_1, vec2<f32>(-0.25f, -0.25f), 0f);
    global_2.member[0i] = bitcast<vec4<u32>>(_e19);
    let _e23 = textureSampleLevel(global, global_1, vec2<f32>(0.6f, 0.2f), 0f);
    global_2.member[1i] = bitcast<vec4<u32>>(_e23);
    let _e27 = textureSampleLevel(global, global_1, vec2<f32>(0.2f, 0.6f), 0f);
    global_2.member[2i] = bitcast<vec4<u32>>(_e27);
    let _e31 = textureSampleLevel(global, global_1, vec2<f32>(1.25f, 1.25f), 0f);
    global_2.member[3i] = bitcast<vec4<u32>>(_e31);
    return;
}

@compute @workgroup_size(1, 1, 1)
fn main() {
    function_();
}

struct type_7 {
    member: vec4<u32>,
}

@group(0) @binding(0)
var global: texture_2d<f32>;
@group(0) @binding(1)
var global_1: sampler;
@group(1) @binding(0)
var<storage, read_write> global_2: type_7;

fn function_() {
    let _e15 = textureSampleLevel(global, global_1, vec2<f32>(-0.25f, -0.25f), 0f);
    let _e17 = textureSampleLevel(global, global_1, vec2<f32>(0.6f, 0.2f), 0f);
    let _e19 = textureSampleLevel(global, global_1, vec2<f32>(0.2f, 0.6f), 0f);
    let _e21 = textureSampleLevel(global, global_1, vec2<f32>(1.25f, 1.25f), 0f);
    global_2.member = vec4<u32>(pack4x8unorm(_e15), pack4x8unorm(_e17), pack4x8unorm(_e19), pack4x8unorm(_e21));
    return;
}

@compute @workgroup_size(1, 1, 1)
fn main() {
    function_();
}

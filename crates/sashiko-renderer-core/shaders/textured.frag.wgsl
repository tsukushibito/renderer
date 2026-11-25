var<private> global: vec4<f32>;
@group(1) @binding(0)
var global_1: texture_2d<f32>;
@group(1) @binding(1)
var global_2: sampler;
var<private> global_3: vec2<f32>;
var<private> global_4: vec4<f32>;

fn function_() {
    let _e6 = global_3;
    let _e7 = textureSampleLevel(global_1, global_2, _e6, 0f);
    let _e8 = global_4;
    global = (_e7 * _e8);
    return;
}

@fragment
fn main(@location(1) param: vec2<f32>, @location(0) param_1: vec4<f32>) -> @location(0) vec4<f32> {
    global_3 = param;
    global_4 = param_1;
    function_();
    let _e5 = global;
    return _e5;
}

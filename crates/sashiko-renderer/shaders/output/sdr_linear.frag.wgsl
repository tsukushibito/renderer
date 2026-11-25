var<private> global: vec4<f32>;
@group(0) @binding(0)
var global_1: texture_2d<f32>;
@group(0) @binding(1)
var global_2: sampler;
var<private> global_3: vec4<f32>;

fn function_() {
    let _e9 = global;
    let _e11 = textureDimensions(global_1, 0i);
    let _e15 = textureSampleLevel(global_1, global_2, (_e9.xy / vec2<f32>(vec2<i32>(_e11))), 0f);
    let _e17 = clamp(_e15.xyz, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    global_3 = vec4<f32>(_e17.x, _e17.y, _e17.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    function_();
    let _e3 = global_3;
    return _e3;
}

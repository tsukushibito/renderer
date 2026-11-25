var<private> global: vec4<f32>;
@group(0) @binding(0)
var global_1: texture_2d<f32>;
@group(0) @binding(1)
var global_2: sampler;
var<private> global_3: vec4<f32>;

fn function_() {
    let _e17 = global;
    let _e19 = textureDimensions(global_1, 0i);
    let _e23 = textureSampleLevel(global_1, global_2, (_e17.xy / vec2<f32>(vec2<i32>(_e19))), 0f);
    let _e25 = clamp(_e23.xyz, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    let _e31 = select((_e25 * 12.92f), ((pow(_e25, vec3<f32>(0.41666666f, 0.41666666f, 0.41666666f)) * 1.055f) - vec3<f32>(0.055f, 0.055f, 0.055f)), (_e25 > vec3<f32>(0.0031308f, 0.0031308f, 0.0031308f)));
    global_3 = vec4<f32>(_e31.x, _e31.y, _e31.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    function_();
    let _e3 = global_3;
    return _e3;
}

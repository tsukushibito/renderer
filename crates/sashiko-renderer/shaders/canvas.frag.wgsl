struct type_u002e_PushConstant_u002e_Rectangle {
    origin: vec2<f32>,
    size: vec2<f32>,
}

@group(0) @binding(255) var<uniform> rect: type_u002e_PushConstant_u002e_Rectangle;
@group(0) @binding(0)
var source: texture_2d<f32>;
@group(0) @binding(1)
var nearest_clamp: sampler;
var<private> global: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    let _e8 = global;
    let _e11 = rect.origin;
    let _e14 = rect.size;
    let _e16 = textureSampleLevel(source, nearest_clamp, ((_e8.xy - _e11) / _e14), 0f);
    out_u002e_var_u002e_SV_Target0_ = _e16;
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

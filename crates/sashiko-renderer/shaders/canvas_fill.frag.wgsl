struct type_u002e_PushConstant_u002e_Fill {
    rgba: vec4<f32>,
}

@group(0) @binding(255) var<uniform> fill: type_u002e_PushConstant_u002e_Fill;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    let _e4 = fill.rgba;
    out_u002e_var_u002e_SV_Target0_ = _e4;
    return;
}

@fragment
fn main() -> @location(0) vec4<f32> {
    main_1();
    let _e1 = out_u002e_var_u002e_SV_Target0_;
    return _e1;
}

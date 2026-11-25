var<private> in_u002e_var_u002e_TEXCOORD0_1: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    let _e2 = in_u002e_var_u002e_TEXCOORD0_1;
    out_u002e_var_u002e_SV_Target0_ = _e2;
    return;
}

@fragment
fn main(@location(0) @interpolate(flat) in_u002e_var_u002e_TEXCOORD0_: vec4<f32>) -> @location(0) vec4<f32> {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

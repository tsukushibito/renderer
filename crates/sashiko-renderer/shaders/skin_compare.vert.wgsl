struct VertexOutput {
    @location(0) @interpolate(flat) member: vec4<f32>,
    @builtin(position) member_1: vec4<f32>,
}

var<private> in_u002e_var_u002e_TEXCOORD0_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD1_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD2_1: vec4<f32>;
var<private> in_u002e_var_u002e_TEXCOORD3_1: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD0_: vec4<f32>;
var<private> global: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    let _e17 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e18 = in_u002e_var_u002e_TEXCOORD1_1;
    let _e19 = in_u002e_var_u002e_TEXCOORD2_1;
    let _e20 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e41 = clamp(((vec3<f32>(0.5f, 0.5f, 0.5f) + ((normalize(_e18) + (normalize(_e19.xyz) * _e19.w)) * 0.2f)) + (_e20.xyx * 0.1f)), vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    out_u002e_var_u002e_TEXCOORD0_ = vec4<f32>(_e41.x, _e41.y, _e41.z, 1f);
    global = vec4<f32>((_e17.x * 0.65f), ((_e17.y - 0.9f) * 0.8f), (0.5f + (_e17.z * 0.2f)), 1f);
    return;
}

@vertex
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec3<f32>, @location(1) in_u002e_var_u002e_TEXCOORD1_: vec3<f32>, @location(2) in_u002e_var_u002e_TEXCOORD2_: vec4<f32>, @location(3) in_u002e_var_u002e_TEXCOORD3_: vec2<f32>) -> VertexOutput {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    in_u002e_var_u002e_TEXCOORD1_1 = in_u002e_var_u002e_TEXCOORD1_;
    in_u002e_var_u002e_TEXCOORD2_1 = in_u002e_var_u002e_TEXCOORD2_;
    in_u002e_var_u002e_TEXCOORD3_1 = in_u002e_var_u002e_TEXCOORD3_;
    main_1();
    let _e11 = global.y;
    global.y = -(_e11);
    let _e13 = out_u002e_var_u002e_TEXCOORD0_;
    let _e14 = global;
    return VertexOutput(_e13, _e14);
}

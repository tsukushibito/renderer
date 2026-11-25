struct type_u002e_ConstantBuffer_u002e_CameraData {
    view_projection: mat4x4<f32>,
    eye: vec4<f32>,
    light_direction: vec4<f32>,
    light_color: vec4<f32>,
    environment: vec4<f32>,
    shadow_view_projection: array<mat4x4<f32>, 4>,
    shadow_splits: vec4<f32>,
    shadow_texel_world: vec4<f32>,
    shadow_depth_scale: vec4<f32>,
    camera_forward_blend: vec4<f32>,
    shadow_bias: vec4<f32>,
}

struct type_u002e_ConstantBuffer_u002e_ShadowParameters {
    cascade_index: u32,
}

struct VertexOutput {
    @location(0) member: vec2<f32>,
    @builtin(position) member_1: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> camera: type_u002e_ConstantBuffer_u002e_CameraData;
@group(0) @binding(255) var<uniform> parameters: type_u002e_ConstantBuffer_u002e_ShadowParameters;
var<private> in_u002e_var_u002e_TEXCOORD0_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD3_1: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD0_: vec2<f32>;
var<private> global: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    let _e10 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e11 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e13 = parameters.cascade_index;
    let _e17 = camera.shadow_view_projection[min(_e13, 3u)];
    out_u002e_var_u002e_TEXCOORD0_ = _e11;
    global = ((((_e17[0] * _e10.x) + (_e17[1] * _e10.y)) + (_e17[2] * _e10.z)) + _e17[3]);
    return;
}

@vertex
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec3<f32>, @location(3) in_u002e_var_u002e_TEXCOORD3_: vec2<f32>) -> VertexOutput {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    in_u002e_var_u002e_TEXCOORD3_1 = in_u002e_var_u002e_TEXCOORD3_;
    main_1();
    let _e7 = global.y;
    global.y = -(_e7);
    let _e9 = out_u002e_var_u002e_TEXCOORD0_;
    let _e10 = global;
    return VertexOutput(_e9, _e10);
}

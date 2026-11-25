struct type_u002e_ConstantBuffer_u002e_SurfacePlacement {
    slot: u32,
}

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

struct VertexOutput {
    @location(0) member: vec3<f32>,
    @location(1) member_1: vec3<f32>,
    @location(2) member_2: vec4<f32>,
    @location(3) member_3: vec2<f32>,
    @location(5) member_4: vec2<f32>,
    @location(6) @interpolate(flat) member_5: u32,
    @builtin(position) member_6: vec4<f32>,
}

@group(0) @binding(255) var<uniform> surface_placement: type_u002e_ConstantBuffer_u002e_SurfacePlacement;
@group(0) @binding(0)
var<uniform> camera: type_u002e_ConstantBuffer_u002e_CameraData;
var<private> in_u002e_var_u002e_TEXCOORD0_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD1_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD2_1: vec4<f32>;
var<private> in_u002e_var_u002e_TEXCOORD3_1: vec2<f32>;
var<private> in_u002e_var_u002e_TEXCOORD4_1: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD0_: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD1_: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD2_: vec4<f32>;
var<private> out_u002e_var_u002e_TEXCOORD3_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD5_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD6_: u32;
var<private> global: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    let _e16 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e17 = in_u002e_var_u002e_TEXCOORD1_1;
    let _e18 = in_u002e_var_u002e_TEXCOORD2_1;
    let _e19 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e20 = in_u002e_var_u002e_TEXCOORD4_1;
    let _e22 = camera.view_projection;
    let _e38 = surface_placement.slot;
    out_u002e_var_u002e_TEXCOORD0_ = _e16;
    out_u002e_var_u002e_TEXCOORD1_ = _e17;
    out_u002e_var_u002e_TEXCOORD2_ = _e18;
    out_u002e_var_u002e_TEXCOORD3_ = _e19;
    out_u002e_var_u002e_TEXCOORD5_ = _e20.xy;
    out_u002e_var_u002e_TEXCOORD6_ = (_e38 + u32(_e20.z));
    global = ((((_e22[0] * _e16.x) + (_e22[1] * _e16.y)) + (_e22[2] * _e16.z)) + _e22[3]);
    return;
}

@vertex
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec3<f32>, @location(1) in_u002e_var_u002e_TEXCOORD1_: vec3<f32>, @location(2) in_u002e_var_u002e_TEXCOORD2_: vec4<f32>, @location(3) in_u002e_var_u002e_TEXCOORD3_: vec2<f32>, @location(4) in_u002e_var_u002e_TEXCOORD4_: vec3<f32>) -> VertexOutput {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    in_u002e_var_u002e_TEXCOORD1_1 = in_u002e_var_u002e_TEXCOORD1_;
    in_u002e_var_u002e_TEXCOORD2_1 = in_u002e_var_u002e_TEXCOORD2_;
    in_u002e_var_u002e_TEXCOORD3_1 = in_u002e_var_u002e_TEXCOORD3_;
    in_u002e_var_u002e_TEXCOORD4_1 = in_u002e_var_u002e_TEXCOORD4_;
    main_1();
    let _e18 = global.y;
    global.y = -(_e18);
    let _e20 = out_u002e_var_u002e_TEXCOORD0_;
    let _e21 = out_u002e_var_u002e_TEXCOORD1_;
    let _e22 = out_u002e_var_u002e_TEXCOORD2_;
    let _e23 = out_u002e_var_u002e_TEXCOORD3_;
    let _e24 = out_u002e_var_u002e_TEXCOORD5_;
    let _e25 = out_u002e_var_u002e_TEXCOORD6_;
    let _e26 = global;
    return VertexOutput(_e20, _e21, _e22, _e23, _e24, _e25, _e26);
}

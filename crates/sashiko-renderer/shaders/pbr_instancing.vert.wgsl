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

struct type_u002e_ConstantBuffer_u002e_Placements {
    source_scale: vec4<f32>,
    origin: vec4<f32>,
    column_step: vec4<f32>,
    row_step: vec4<f32>,
    instance_offset: u32,
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

@group(0) @binding(0)
var<uniform> camera: type_u002e_ConstantBuffer_u002e_CameraData;
@group(0) @binding(255) var<uniform> placements: type_u002e_ConstantBuffer_u002e_Placements;
var<private> in_u002e_var_u002e_TEXCOORD0_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD1_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD2_1: vec4<f32>;
var<private> in_u002e_var_u002e_TEXCOORD3_1: vec2<f32>;
var<private> global: u32;
var<private> out_u002e_var_u002e_TEXCOORD0_: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD1_: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD2_: vec4<f32>;
var<private> out_u002e_var_u002e_TEXCOORD3_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD5_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD6_: u32;
var<private> global_1: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    let _e25 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e26 = in_u002e_var_u002e_TEXCOORD1_1;
    let _e27 = in_u002e_var_u002e_TEXCOORD2_1;
    let _e28 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e29 = global;
    let _e32 = placements.instance_offset;
    let _e35 = min((min(_e29, 23u) + min(_e32, 23u)), 23u);
    let _e37 = placements.source_scale;
    let _e42 = placements.source_scale[3i];
    let _e45 = placements.column_step;
    let _e51 = placements.row_step;
    let _e57 = placements.origin;
    let _e61 = (((_e57.xyz + (_e45.xyz * f32((_e35 % 6u)))) + (_e51.xyz * f32((_e35 / 6u)))) + ((_e25 - _e37.xyz) * _e42));
    let _e63 = camera.view_projection;
    out_u002e_var_u002e_TEXCOORD0_ = _e61;
    out_u002e_var_u002e_TEXCOORD1_ = _e26;
    out_u002e_var_u002e_TEXCOORD2_ = _e27;
    out_u002e_var_u002e_TEXCOORD3_ = _e28;
    out_u002e_var_u002e_TEXCOORD5_ = vec2<f32>(0f, 0f);
    out_u002e_var_u002e_TEXCOORD6_ = 0u;
    global_1 = ((((_e63[0] * _e61.x) + (_e63[1] * _e61.y)) + (_e63[2] * _e61.z)) + _e63[3]);
    return;
}

@vertex
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec3<f32>, @location(1) in_u002e_var_u002e_TEXCOORD1_: vec3<f32>, @location(2) in_u002e_var_u002e_TEXCOORD2_: vec4<f32>, @location(3) in_u002e_var_u002e_TEXCOORD3_: vec2<f32>, @builtin(instance_index) param: u32) -> VertexOutput {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    in_u002e_var_u002e_TEXCOORD1_1 = in_u002e_var_u002e_TEXCOORD1_;
    in_u002e_var_u002e_TEXCOORD2_1 = in_u002e_var_u002e_TEXCOORD2_;
    in_u002e_var_u002e_TEXCOORD3_1 = in_u002e_var_u002e_TEXCOORD3_;
    global = param;
    main_1();
    let _e18 = global_1.y;
    global_1.y = -(_e18);
    let _e20 = out_u002e_var_u002e_TEXCOORD0_;
    let _e21 = out_u002e_var_u002e_TEXCOORD1_;
    let _e22 = out_u002e_var_u002e_TEXCOORD2_;
    let _e23 = out_u002e_var_u002e_TEXCOORD3_;
    let _e24 = out_u002e_var_u002e_TEXCOORD5_;
    let _e25 = out_u002e_var_u002e_TEXCOORD6_;
    let _e26 = global_1;
    return VertexOutput(_e20, _e21, _e22, _e23, _e24, _e25, _e26);
}

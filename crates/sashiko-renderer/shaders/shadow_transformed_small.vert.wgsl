struct type_u002e_ByteAddressBuffer {
    member: array<u32>,
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

struct type_u002e_ConstantBuffer_u002e_Parameters {
    cascade_index: u32,
    first_transform: u32,
}

struct VertexOutput {
    @location(0) member: vec2<f32>,
    @location(1) @interpolate(flat) member_1: u32,
    @builtin(position) member_2: vec4<f32>,
}

@group(0) @binding(4)
var<storage> transform_table: type_u002e_ByteAddressBuffer;
@group(0) @binding(0)
var<uniform> camera: type_u002e_ConstantBuffer_u002e_CameraData;
@group(0) @binding(255) var<uniform> parameters: type_u002e_ConstantBuffer_u002e_Parameters;
var<private> in_u002e_var_u002e_TEXCOORD0_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD3_1: vec2<f32>;
var<private> global: u32;
var<private> out_u002e_var_u002e_TEXCOORD0_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD1_: u32;
var<private> global_1: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    let _e23 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e24 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e25 = global;
    let _e27 = parameters.cascade_index;
    let _e30 = parameters.first_transform;
    let _e31 = min(_e30, 63u);
    let _e35 = ((_e31 + min(_e25, (63u - _e31))) * 96u);
    let _e37 = (_e35 >> bitcast<u32>(2u));
    let _e40 = transform_table.member[_e37];
    let _e44 = transform_table.member[(_e37 + 1u)];
    let _e48 = transform_table.member[(_e37 + 2u)];
    let _e52 = transform_table.member[(_e37 + 3u)];
    let _e54 = bitcast<vec4<f32>>(vec4<u32>(_e40, _e44, _e48, _e52));
    let _e57 = ((_e35 + 16u) >> bitcast<u32>(2u));
    let _e60 = transform_table.member[_e57];
    let _e64 = transform_table.member[(_e57 + 1u)];
    let _e68 = transform_table.member[(_e57 + 2u)];
    let _e72 = transform_table.member[(_e57 + 3u)];
    let _e74 = bitcast<vec4<f32>>(vec4<u32>(_e60, _e64, _e68, _e72));
    let _e77 = ((_e35 + 32u) >> bitcast<u32>(2u));
    let _e80 = transform_table.member[_e77];
    let _e84 = transform_table.member[(_e77 + 1u)];
    let _e88 = transform_table.member[(_e77 + 2u)];
    let _e92 = transform_table.member[(_e77 + 3u)];
    let _e94 = bitcast<vec4<f32>>(vec4<u32>(_e80, _e84, _e88, _e92));
    let _e119 = ((((vec3<f32>(_e54.x, _e74.x, _e94.x) * _e23.x) + (vec3<f32>(_e54.y, _e74.y, _e94.y) * _e23.y)) + (vec3<f32>(_e54.z, _e74.z, _e94.z) * _e23.z)) + vec3<f32>(_e54.w, _e74.w, _e94.w));
    let _e122 = camera.shadow_view_projection[min(_e27, 3u)];
    let _e141 = transform_table.member[((_e35 + 60u) >> bitcast<u32>(2u))];
    out_u002e_var_u002e_TEXCOORD0_ = _e24;
    out_u002e_var_u002e_TEXCOORD1_ = select(0u, 1u, (bitcast<f32>(_e141) < 0f));
    global_1 = ((((_e122[0] * _e119.x) + (_e122[1] * _e119.y)) + (_e122[2] * _e119.z)) + _e122[3]);
    return;
}

@vertex
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec3<f32>, @location(3) in_u002e_var_u002e_TEXCOORD3_: vec2<f32>, @builtin(instance_index) param: u32) -> VertexOutput {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    in_u002e_var_u002e_TEXCOORD3_1 = in_u002e_var_u002e_TEXCOORD3_;
    global = param;
    main_1();
    let _e10 = global_1.y;
    global_1.y = -(_e10);
    let _e12 = out_u002e_var_u002e_TEXCOORD0_;
    let _e13 = out_u002e_var_u002e_TEXCOORD1_;
    let _e14 = global_1;
    return VertexOutput(_e12, _e13, _e14);
}

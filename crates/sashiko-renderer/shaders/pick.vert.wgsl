struct type_u002e_ConstantBuffer_u002e_PickParameters {
    clip_from_world: mat4x4<f32>,
    model_rows: array<vec4<f32>, 3>,
    id: u32,
}

struct VertexOutput {
    @location(0) @interpolate(flat) member: vec4<f32>,
    @builtin(position) member_1: vec4<f32>,
}

@group(0) @binding(255) var<uniform> pick: type_u002e_ConstantBuffer_u002e_PickParameters;
var<private> in_u002e_var_u002e_TEXCOORD0_1: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD0_: vec4<f32>;
var<private> global: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    let _e18 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e22 = vec4<f32>(_e18.x, _e18.y, _e18.z, 1f);
    let _e25 = pick.model_rows[0i];
    let _e29 = pick.model_rows[1i];
    let _e33 = pick.model_rows[2i];
    let _e36 = pick.clip_from_world;
    let _e48 = pick.id;
    out_u002e_var_u002e_TEXCOORD0_ = (vec4<f32>(((vec4(_e48) >> bitcast<vec4<u32>>(vec4<u32>(0u, 8u, 16u, 24u))) & vec4<u32>(255u, 255u, 255u, 255u))) * vec4<f32>(0.003921569f, 0.003921569f, 0.003921569f, 0.003921569f));
    global = ((((_e36[0] * dot(_e25, _e22)) + (_e36[1] * dot(_e29, _e22))) + (_e36[2] * dot(_e33, _e22))) + _e36[3]);
    return;
}

@vertex
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec3<f32>) -> VertexOutput {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    main_1();
    let _e5 = global.y;
    global.y = -(_e5);
    let _e7 = out_u002e_var_u002e_TEXCOORD0_;
    let _e8 = global;
    return VertexOutput(_e7, _e8);
}

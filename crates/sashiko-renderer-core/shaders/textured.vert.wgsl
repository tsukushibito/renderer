struct type_4 {
    @builtin(position) member: vec4<f32>,
    member_1: f32,
    member_2: array<f32, 1>,
    member_3: array<f32, 1>,
}

struct type_8 {
    member: mat4x4<f32>,
    member_1: vec4<f32>,
}

struct type_13 {
    member: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) member: vec4<f32>,
    @location(0) member_1: vec4<f32>,
    @location(1) member_2: vec2<f32>,
}

var<private> global: type_4 = type_4(vec4<f32>(0f, 0f, 0f, 1f), 1f, array<f32, 1>(), array<f32, 1>());
@group(0) @binding(255) var<uniform> global_1: type_8;
var<private> global_2: vec3<f32>;
@group(0) @binding(0)
var<uniform> global_3: type_13;
var<private> global_4: vec4<f32>;
var<private> global_5: vec4<f32>;
var<private> global_6: vec2<f32>;

fn function_() {
    let _e14 = global_1.member;
    let _e15 = global_2;
    let _e22 = global_3.member;
    global.member = ((_e14 * vec4<f32>(_e15.x, _e15.y, _e15.z, 1f)) + _e22);
    let _e25 = global_5;
    let _e27 = global_1.member_1;
    global_4 = (_e25 * _e27);
    global_6 = ((_e15.xy * 0.5f) + vec2<f32>(0.5f, 0.5f));
    return;
}

@vertex
fn main(@location(0) param: vec3<f32>, @location(1) param_1: vec4<f32>) -> VertexOutput {
    global_2 = param;
    global_5 = param_1;
    function_();
    let _e9 = global.member.y;
    global.member.y = -(_e9);
    let _e11 = global.member;
    let _e12 = global_4;
    let _e13 = global_6;
    return VertexOutput(_e11, _e12, _e13);
}

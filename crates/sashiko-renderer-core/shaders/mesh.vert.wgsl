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

struct VertexOutput {
    @builtin(position) member: vec4<f32>,
    @location(0) member_1: vec4<f32>,
}

var<private> global: type_4 = type_4(vec4<f32>(0f, 0f, 0f, 1f), 1f, array<f32, 1>(), array<f32, 1>());
@group(0) @binding(255) var<uniform> global_1: type_8;
var<private> global_2: vec3<f32>;
var<private> global_3: vec4<f32>;
var<private> global_4: vec4<f32>;

fn function_() {
    let _e10 = global_1.member;
    let _e11 = global_2;
    global.member = (_e10 * vec4<f32>(_e11.x, _e11.y, _e11.z, 1f));
    let _e18 = global_4;
    let _e20 = global_1.member_1;
    global_3 = (_e18 * _e20);
    return;
}

@vertex
fn main(@location(0) param: vec3<f32>, @location(1) param_1: vec4<f32>) -> VertexOutput {
    global_2 = param;
    global_4 = param_1;
    function_();
    let _e8 = global.member.y;
    global.member.y = -(_e8);
    let _e10 = global.member;
    let _e11 = global_3;
    return VertexOutput(_e10, _e11);
}

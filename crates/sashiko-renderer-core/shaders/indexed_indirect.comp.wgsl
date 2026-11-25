struct type_6 {
    member: array<vec2<u32>>,
}

struct type_11 {
    member: array<u32>,
}

struct type_15 {
    member: u32,
    member_1: u32,
    member_2: u32,
    member_3: i32,
    member_4: u32,
}

struct type_17 {
    member: array<type_15>,
}

var<private> global: vec3<u32>;
@group(0) @binding(0)
var<storage> global_1: type_6;
@group(0) @binding(1)
var<storage> global_2: type_11;
@group(0) @binding(2)
var<storage, read_write> global_3: type_17;

fn function_() {
    let _e14 = global[0u];
    let _e17 = global_1.member[_e14];
    let _e20 = global_2.member[_e14];
    global_3.member[_e14].member = _e17.y;
    global_3.member[_e14].member_1 = select(0u, 1u, (_e20 != 0u));
    global_3.member[_e14].member_2 = _e17.x;
    global_3.member[_e14].member_3 = 0i;
    global_3.member[_e14].member_4 = 0u;
    return;
}

@compute @workgroup_size(64, 1, 1)
fn main(@builtin(global_invocation_id) param: vec3<u32>) {
    global = param;
    function_();
}

struct type_2 {
    member: vec4<u32>,
}

@group(1) @binding(0)
var<storage, read_write> global: type_2;
@group(0) @binding(0)
var<uniform> global_1: type_2;

fn function_() {
    let _e6 = global_1.member;
    global.member = _e6;
    return;
}

@compute @workgroup_size(1, 1, 1)
fn main() {
    function_();
}

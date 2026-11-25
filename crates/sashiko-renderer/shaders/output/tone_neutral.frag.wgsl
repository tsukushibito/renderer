struct type_11 {
    member: f32,
}

var<private> global: vec4<f32>;
@group(0) @binding(0)
var global_1: texture_2d<f32>;
@group(0) @binding(1)
var global_2: sampler;
var<private> global_3: vec4<f32>;
@group(0) @binding(255) var<uniform> global_4: type_11;

fn function_() {
    var phi_249_: f32;
    var phi_251_: vec3<f32>;

    let _e19 = global;
    let _e21 = textureDimensions(global_1, 0i);
    let _e25 = textureSampleLevel(global_1, global_2, (_e19.xy / vec2<f32>(vec2<i32>(_e21))), 0f);
    let _e29 = global_4.member;
    let _e30 = (max(_e25.xyz, vec3<f32>(0f, 0f, 0f)) * _e29);
    switch bitcast<i32>(0u) {
        default: {
            let _e36 = min(_e30.x, min(_e30.y, _e30.z));
            if (_e36 < 0.08f) {
                phi_249_ = (_e36 - ((6.25f * _e36) * _e36));
            } else {
                phi_249_ = 0.04f;
            }
            let _e42 = phi_249_;
            let _e44 = (_e30 - vec3(_e42));
            let _e49 = max(_e44.x, max(_e44.y, _e44.z));
            if (_e49 < 0.76f) {
                phi_251_ = _e44;
                break;
            }
            let _e53 = (1f - (0.0576f / (_e49 + -0.52f)));
            phi_251_ = mix((_e44 * (_e53 / max(_e49, 0.00000000000000000001f))), vec3(_e53), vec3((1f - (1f / ((0.15f * (_e49 - _e53)) + 1f)))));
            break;
        }
    }
    let _e66 = phi_251_;
    let _e67 = clamp(_e66, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    global_3 = vec4<f32>(_e67.x, _e67.y, _e67.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    function_();
    let _e3 = global_3;
    return _e3;
}

struct type_u002e_ConstantBuffer_u002e_ToneParameters {
    exposure: f32,
}

@group(0) @binding(0)
var linear_image: texture_2d<f32>;
@group(0) @binding(1)
var source_sampler: sampler;
@group(0) @binding(255) var<uniform> tone: type_u002e_ConstantBuffer_u002e_ToneParameters;
var<private> global: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    var phi_76_: f32;
    var phi_101_: vec3<f32>;

    let _e19 = global;
    let _e21 = textureDimensions(linear_image, 0i);
    let _e28 = textureSampleLevel(linear_image, source_sampler, (_e19.xy / vec2<f32>(f32(_e21.x), f32(_e21.y))), 0f);
    let _e32 = tone.exposure;
    let _e33 = (max(_e28.xyz, vec3<f32>(0f, 0f, 0f)) * _e32);
    switch bitcast<i32>(0u) {
        default: {
            let _e39 = min(_e33.x, min(_e33.y, _e33.z));
            if (_e39 < 0.08f) {
                phi_76_ = (_e39 - ((6.25f * _e39) * _e39));
            } else {
                phi_76_ = 0.04f;
            }
            let _e45 = phi_76_;
            let _e47 = (_e33 - vec3(_e45));
            let _e52 = max(_e47.x, max(_e47.y, _e47.z));
            if (_e52 < 0.76f) {
                phi_101_ = _e47;
                break;
            }
            let _e56 = (1f - (0.057600003f / (_e52 + -0.52f)));
            phi_101_ = mix((_e47 * (_e56 / max(_e52, 0.00000000000000000001f))), vec3(_e56), vec3((1f - (1f / ((0.15f * (_e52 - _e56)) + 1f)))));
            break;
        }
    }
    let _e69 = phi_101_;
    let _e70 = clamp(_e69, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    out_u002e_var_u002e_SV_Target0_ = vec4<f32>(_e70.x, _e70.y, _e70.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

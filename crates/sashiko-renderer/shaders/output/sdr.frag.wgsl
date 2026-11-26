@group(0) @binding(0)
var linear_image: texture_2d<f32>;
@group(0) @binding(1)
var source_sampler: sampler;
var<private> global: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    let _e17 = global;
    let _e19 = textureDimensions(linear_image, 0i);
    let _e26 = textureSampleLevel(linear_image, source_sampler, (_e17.xy / vec2<f32>(f32(_e19.x), f32(_e19.y))), 0f);
    let _e28 = clamp(_e26.xyz, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    let _e34 = select((_e28 * 12.92f), ((pow(_e28, vec3<f32>(0.41666666f, 0.41666666f, 0.41666666f)) * 1.055f) - vec3<f32>(0.055f, 0.055f, 0.055f)), (_e28 > vec3<f32>(0.0031308f, 0.0031308f, 0.0031308f)));
    out_u002e_var_u002e_SV_Target0_ = vec4<f32>(_e34.x, _e34.y, _e34.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

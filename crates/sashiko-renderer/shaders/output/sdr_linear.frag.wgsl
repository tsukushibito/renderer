@group(0) @binding(0)
var linear_image: texture_2d<f32>;
@group(0) @binding(1)
var source_sampler: sampler;
var<private> global: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    let _e9 = global;
    let _e11 = textureDimensions(linear_image, 0i);
    let _e18 = textureSampleLevel(linear_image, source_sampler, (_e9.xy / vec2<f32>(f32(_e11.x), f32(_e11.y))), 0f);
    let _e20 = clamp(_e18.xyz, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    out_u002e_var_u002e_SV_Target0_ = vec4<f32>(_e20.x, _e20.y, _e20.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

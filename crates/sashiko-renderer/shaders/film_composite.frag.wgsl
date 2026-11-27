struct type_u002e_ConstantBuffer_u002e_FilmParameters {
    corner_scale: vec4<f32>,
    step_x: vec4<f32>,
    step_y: vec4<f32>,
    sensor_r: vec4<f32>,
    sensor_g: vec4<f32>,
    sensor_b: vec4<f32>,
}

@group(0) @binding(0)
var scene: texture_2d<f32>;
@group(0) @binding(1)
var sky: texture_2d<f32>;
@group(0) @binding(255) var<uniform> film: type_u002e_ConstantBuffer_u002e_FilmParameters;
var<private> global: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    var phi_92_: bool;
    var phi_101_: f32;
    var phi_110_: vec2<f32>;
    var phi_141_: vec3<f32>;

    let _e20 = global;
    let _e22 = vec2<i32>(_e20.xy);
    let _e27 = textureLoad(scene, vec3<i32>(_e22.x, _e22.y, 0i).xy, 0i);
    let _e28 = _e27.xyz;
    phi_141_ = _e28;
    if (_e27.w < 1f) {
        let _e31 = textureDimensions(sky, 0i);
        let _e35 = film.corner_scale;
        let _e38 = film.step_x;
        let _e44 = film.step_y;
        let _e49 = normalize(((_e35.xyz + (_e38.xyz * _e20.x)) + (_e44.xyz * _e20.y)));
        phi_92_ = false;
        if (_e49.x == 0f) {
            phi_92_ = (_e49.y == 0f);
        }
        let _e63 = phi_92_;
        if _e63 {
            phi_101_ = 0f;
        } else {
            phi_101_ = (atan2(abs(_e49.y), abs(_e49.x)) * 0.63661975f);
        }
        let _e70 = phi_101_;
        let _e73 = (vec2<f32>((1f - _e70), _e70) * sqrt(max(0f, (1f - abs(_e49.z)))));
        phi_110_ = _e73;
        if (_e49.z < 0f) {
            phi_110_ = (vec2<f32>(1f, 1f) - _e73.yx);
        }
        let _e78 = phi_110_;
        let _e93 = min(vec2<u32>((_e31.x - 1u), (_e31.y - 1u)), vec2<u32>(((vec2<f32>(0.5f, 0.5f) + ((_e78 * 0.5f) * vec2<f32>(select(1f, -1f, (_e49.x < 0f)), select(1f, -1f, (_e49.y < 0f))))) * vec2<f32>(f32(_e31.x), f32(_e31.y)))));
        let _e100 = textureLoad(sky, vec3<i32>(bitcast<i32>(_e93.x), bitcast<i32>(_e93.y), 0i).xy, 0i);
        let _e105 = film.corner_scale[3i];
        phi_141_ = (_e28 + (_e100.xyz * ((1f - _e27.w) * _e105)));
    }
    let _e110 = phi_141_;
    let _e112 = film.sensor_r;
    let _e116 = film.sensor_g;
    let _e120 = film.sensor_b;
    out_u002e_var_u002e_SV_Target0_ = vec4<f32>(dot(_e112.xyz, _e110), dot(_e116.xyz, _e110), dot(_e120.xyz, _e110), 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

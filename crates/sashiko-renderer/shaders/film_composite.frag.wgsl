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
    var phi_94_: bool;
    var phi_103_: f32;
    var phi_112_: vec2<f32>;
    var phi_143_: vec3<f32>;

    let _e22 = global;
    let _e24 = vec2<i32>(_e22.xy);
    let _e29 = textureLoad(scene, vec3<i32>(_e24.x, _e24.y, 0i).xy, 0i);
    let _e30 = _e29.xyz;
    phi_143_ = _e30;
    if (_e29.w < 1f) {
        let _e33 = textureDimensions(sky, 0i);
        let _e37 = film.corner_scale;
        let _e40 = film.step_x;
        let _e46 = film.step_y;
        let _e51 = normalize(((_e37.xyz + (_e40.xyz * _e22.x)) + (_e46.xyz * _e22.y)));
        phi_94_ = false;
        if (_e51.x == 0f) {
            phi_94_ = (_e51.y == 0f);
        }
        let _e65 = phi_94_;
        if _e65 {
            phi_103_ = 0f;
        } else {
            phi_103_ = (atan2(abs(_e51.y), abs(_e51.x)) * 0.63661975f);
        }
        let _e72 = phi_103_;
        let _e75 = (vec2<f32>((1f - _e72), _e72) * sqrt(max(0f, (1f - abs(_e51.z)))));
        phi_112_ = _e75;
        if (_e51.z < 0f) {
            phi_112_ = (vec2<f32>(1f, 1f) - _e75.yx);
        }
        let _e80 = phi_112_;
        let _e95 = min(vec2<u32>((_e33.x - 1u), (_e33.y - 1u)), vec2<u32>(((vec2<f32>(0.5f, 0.5f) + ((_e80 * 0.5f) * vec2<f32>(select(1f, -1f, (_e51.x < 0f)), select(1f, -1f, (_e51.y < 0f))))) * vec2<f32>(f32(_e33.x), f32(_e33.y)))));
        let _e102 = textureLoad(sky, vec3<i32>(bitcast<i32>(_e95.x), bitcast<i32>(_e95.y), 0i).xy, 0i);
        let _e107 = film.corner_scale[3i];
        phi_143_ = (_e30 + (_e102.xyz * ((1f - _e29.w) * _e107)));
    }
    let _e112 = phi_143_;
    let _e114 = film.sensor_r;
    let _e118 = film.sensor_g;
    let _e122 = film.sensor_b;
    let _e126 = clamp(vec3<f32>(dot(_e114.xyz, _e112), dot(_e118.xyz, _e112), dot(_e122.xyz, _e112)), vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    out_u002e_var_u002e_SV_Target0_ = vec4<f32>(_e126.x, _e126.y, _e126.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

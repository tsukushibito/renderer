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
@group(0) @binding(2)
var sky_linear: sampler;
@group(0) @binding(255) var<uniform> film: type_u002e_ConstantBuffer_u002e_FilmParameters;
var<private> global: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    var phi_99_: bool;
    var phi_108_: f32;
    var phi_117_: vec2<f32>;
    var phi_160_: bool;
    var phi_176_: bool;
    var phi_185_: vec2<i32>;
    var phi_191_: bool;
    var phi_200_: vec2<i32>;
    var phi_214_: bool;
    var phi_223_: vec2<i32>;
    var phi_229_: bool;
    var phi_238_: vec2<i32>;
    var phi_255_: bool;
    var phi_264_: vec2<i32>;
    var phi_270_: bool;
    var phi_279_: vec2<i32>;
    var phi_293_: bool;
    var phi_302_: vec2<i32>;
    var phi_308_: bool;
    var phi_317_: vec2<i32>;
    var phi_329_: vec3<f32>;
    var phi_336_: vec3<f32>;

    let _e26 = global;
    let _e28 = vec2<i32>(_e26.xy);
    let _e33 = textureLoad(scene, vec3<i32>(_e28.x, _e28.y, 0i).xy, 0i);
    let _e34 = _e33.xyz;
    phi_336_ = _e34;
    if (_e33.w < 1f) {
        let _e37 = textureDimensions(sky, 0i);
        let _e41 = film.corner_scale;
        let _e44 = film.step_x;
        let _e50 = film.step_y;
        let _e55 = normalize(((_e41.xyz + (_e44.xyz * _e26.x)) + (_e50.xyz * _e26.y)));
        phi_99_ = false;
        if (_e55.x == 0f) {
            phi_99_ = (_e55.y == 0f);
        }
        let _e66 = phi_99_;
        if _e66 {
            phi_108_ = 0f;
        } else {
            phi_108_ = (atan2(abs(_e55.y), abs(_e55.x)) * 0.63661975f);
        }
        let _e73 = phi_108_;
        let _e76 = (vec2<f32>((1f - _e73), _e73) * sqrt(max(0f, (1f - abs(_e55.z)))));
        phi_117_ = _e76;
        if (_e55.z < 0f) {
            phi_117_ = (vec2<f32>(1f, 1f) - _e76.yx);
        }
        let _e81 = phi_117_;
        let _e90 = (vec2<f32>(0.5f, 0.5f) + ((_e81 * 0.5f) * vec2<f32>(select(1f, -1f, (_e55.x < 0f)), select(1f, -1f, (_e55.y < 0f)))));
        let _e91 = bitcast<i32>(_e37.x);
        let _e92 = bitcast<i32>(_e37.y);
        let _e93 = vec2<i32>(_e91, _e92);
        switch bitcast<i32>(0u) {
            default: {
                let _e97 = film.step_x[3i];
                if (_e97 == 0f) {
                    let _e103 = min(vec2<i32>((_e90 * vec2<f32>(_e93))), (_e93 - vec2<i32>(1i, 1i)));
                    let _e108 = textureLoad(sky, vec3<i32>(_e103.x, _e103.y, 0i).xy, 0i);
                    phi_329_ = _e108.xyz;
                    break;
                }
                let _e112 = ((_e90 * vec2<f32>(_e93)) - vec2<f32>(0.5f, 0.5f));
                phi_160_ = false;
                if all((_e112 >= vec2<f32>(0f, 0f))) {
                    phi_160_ = all((_e112 < vec2<f32>((_e93 - vec2<i32>(1i, 1i)))));
                }
                let _e120 = phi_160_;
                if _e120 {
                    let _e121 = textureSampleLevel(sky, sky_linear, _e90, 0f);
                    phi_329_ = _e121.xyz;
                    break;
                }
                let _e124 = vec2<i32>(floor(_e112));
                let _e125 = fract(_e112);
                phi_176_ = true;
                if (_e124.x >= 0i) {
                    phi_176_ = (_e124.x >= _e91);
                }
                let _e130 = phi_176_;
                phi_185_ = _e124;
                if _e130 {
                    phi_185_ = vec2<i32>(clamp(_e124.x, 0i, (_e91 - 1i)), ((_e92 - 1i) - _e124.y));
                }
                let _e138 = phi_185_;
                phi_191_ = true;
                if (_e138.y >= 0i) {
                    phi_191_ = (_e138.y >= _e92);
                }
                let _e143 = phi_191_;
                phi_200_ = _e138;
                if _e143 {
                    phi_200_ = vec2<i32>(((_e91 - 1i) - _e138.x), clamp(_e138.y, 0i, (_e92 - 1i)));
                }
                let _e151 = phi_200_;
                let _e156 = textureLoad(sky, vec3<i32>(_e151.x, _e151.y, 0i).xy, 0i);
                let _e158 = (_e124 + vec2<i32>(1i, 0i));
                phi_214_ = true;
                if (_e158.x >= 0i) {
                    phi_214_ = (_e158.x >= _e91);
                }
                let _e163 = phi_214_;
                phi_223_ = _e158;
                if _e163 {
                    phi_223_ = vec2<i32>(clamp(_e158.x, 0i, (_e91 - 1i)), ((_e92 - 1i) - _e158.y));
                }
                let _e171 = phi_223_;
                phi_229_ = true;
                if (_e171.y >= 0i) {
                    phi_229_ = (_e171.y >= _e92);
                }
                let _e176 = phi_229_;
                phi_238_ = _e171;
                if _e176 {
                    phi_238_ = vec2<i32>(((_e91 - 1i) - _e171.x), clamp(_e171.y, 0i, (_e92 - 1i)));
                }
                let _e184 = phi_238_;
                let _e189 = textureLoad(sky, vec3<i32>(_e184.x, _e184.y, 0i).xy, 0i);
                let _e192 = vec3(_e125.x);
                let _e194 = (_e124 + vec2<i32>(0i, 1i));
                phi_255_ = true;
                if (_e194.x >= 0i) {
                    phi_255_ = (_e194.x >= _e91);
                }
                let _e199 = phi_255_;
                phi_264_ = _e194;
                if _e199 {
                    phi_264_ = vec2<i32>(clamp(_e194.x, 0i, (_e91 - 1i)), ((_e92 - 1i) - _e194.y));
                }
                let _e207 = phi_264_;
                phi_270_ = true;
                if (_e207.y >= 0i) {
                    phi_270_ = (_e207.y >= _e92);
                }
                let _e212 = phi_270_;
                phi_279_ = _e207;
                if _e212 {
                    phi_279_ = vec2<i32>(((_e91 - 1i) - _e207.x), clamp(_e207.y, 0i, (_e92 - 1i)));
                }
                let _e220 = phi_279_;
                let _e225 = textureLoad(sky, vec3<i32>(_e220.x, _e220.y, 0i).xy, 0i);
                let _e227 = (_e124 + vec2<i32>(1i, 1i));
                phi_293_ = true;
                if (_e227.x >= 0i) {
                    phi_293_ = (_e227.x >= _e91);
                }
                let _e232 = phi_293_;
                phi_302_ = _e227;
                if _e232 {
                    phi_302_ = vec2<i32>(clamp(_e227.x, 0i, (_e91 - 1i)), ((_e92 - 1i) - _e227.y));
                }
                let _e240 = phi_302_;
                phi_308_ = true;
                if (_e240.y >= 0i) {
                    phi_308_ = (_e240.y >= _e92);
                }
                let _e245 = phi_308_;
                phi_317_ = _e240;
                if _e245 {
                    phi_317_ = vec2<i32>(((_e91 - 1i) - _e240.x), clamp(_e240.y, 0i, (_e92 - 1i)));
                }
                let _e253 = phi_317_;
                let _e258 = textureLoad(sky, vec3<i32>(_e253.x, _e253.y, 0i).xy, 0i);
                phi_329_ = mix(mix(_e156.xyz, _e189.xyz, _e192), mix(_e225.xyz, _e258.xyz, _e192), vec3(_e125.y));
                break;
            }
        }
        let _e265 = phi_329_;
        let _e269 = film.corner_scale[3i];
        phi_336_ = (_e34 + (_e265 * ((1f - _e33.w) * _e269)));
    }
    let _e274 = phi_336_;
    let _e276 = film.sensor_r;
    let _e280 = film.sensor_g;
    let _e284 = film.sensor_b;
    out_u002e_var_u002e_SV_Target0_ = vec4<f32>(dot(_e276.xyz, _e274), dot(_e280.xyz, _e274), dot(_e284.xyz, _e274), 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

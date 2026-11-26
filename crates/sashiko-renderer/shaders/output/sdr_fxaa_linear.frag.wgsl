@group(0) @binding(0)
var linear_image: texture_2d<f32>;
@group(0) @binding(1)
var source_sampler: sampler;
var<private> global: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    var phi_208_: f32;
    var phi_226_: f32;
    var phi_227_: f32;
    var phi_239_: vec2<f32>;
    var phi_247_: vec2<f32>;
    var phi_278_: vec2<f32>;
    var phi_283_: vec2<f32>;
    var phi_288_: f32;
    var phi_291_: f32;
    var phi_293_: vec2<f32>;
    var phi_295_: bool;
    var phi_297_: vec2<f32>;
    var phi_299_: bool;
    var phi_301_: i32;
    var phi_292_: f32;
    var phi_289_: f32;
    var phi_351_: f32;
    var phi_352_: f32;
    var phi_353_: f32;
    var phi_298_: vec2<f32>;
    var phi_374_: f32;
    var phi_375_: f32;
    var phi_376_: f32;
    var phi_294_: vec2<f32>;
    var phi_381_: f32;
    var phi_382_: f32;
    var phi_383_: vec2<f32>;
    var phi_384_: vec2<f32>;
    var phi_385_: f32;
    var phi_386_: f32;
    var phi_387_: vec2<f32>;
    var phi_388_: vec2<f32>;
    var phi_398_: f32;
    var phi_408_: f32;
    var phi_451_: vec2<f32>;
    var phi_457_: vec3<f32>;
    var local: f32;
    var local_1: f32;
    var local_2: vec2<f32>;
    var local_3: bool;
    var local_4: vec2<f32>;
    var local_5: bool;

    let _e44 = global;
    let _e46 = textureDimensions(linear_image, 0i);
    let _e52 = (_e44.xy / vec2<f32>(f32(_e46.x), f32(_e46.y)));
    let _e53 = textureSampleLevel(linear_image, source_sampler, _e52, 0f);
    let _e55 = clamp(_e53.xyz, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    switch bitcast<i32>(0u) {
        default: {
            let _e57 = textureDimensions(linear_image, 0i);
            let _e63 = (vec2<f32>(1f, 1f) / vec2<f32>(f32(_e57.x), f32(_e57.y)));
            let _e64 = textureSampleLevel(linear_image, source_sampler, _e52, 0f, vec2<i32>(0i, 1i));
            let _e68 = sqrt(max(dot(_e64.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e69 = textureSampleLevel(linear_image, source_sampler, _e52, 0f, vec2<i32>(0i, -1i));
            let _e73 = sqrt(max(dot(_e69.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e74 = textureSampleLevel(linear_image, source_sampler, _e52, 0f, vec2<i32>(-1i, 0i));
            let _e78 = sqrt(max(dot(_e74.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e79 = textureSampleLevel(linear_image, source_sampler, _e52, 0f, vec2<i32>(1i, 0i));
            let _e83 = sqrt(max(dot(_e79.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e86 = sqrt(max(dot(_e55, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e94 = max(_e86, max(max(_e68, _e73), max(_e78, _e83)));
            let _e95 = (_e94 - min(_e86, min(min(_e68, _e73), min(_e78, _e83))));
            if (_e95 < max(0.0312f, (_e94 * 0.125f))) {
                phi_457_ = _e55;
                break;
            }
            let _e99 = textureSampleLevel(linear_image, source_sampler, _e52, 0f, vec2<i32>(-1i, -1i));
            let _e103 = sqrt(max(dot(_e99.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e104 = textureSampleLevel(linear_image, source_sampler, _e52, 0f, vec2<i32>(1i, 1i));
            let _e108 = sqrt(max(dot(_e104.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e109 = textureSampleLevel(linear_image, source_sampler, _e52, 0f, vec2<i32>(-1i, 1i));
            let _e113 = sqrt(max(dot(_e109.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e114 = textureSampleLevel(linear_image, source_sampler, _e52, 0f, vec2<i32>(1i, -1i));
            let _e118 = sqrt(max(dot(_e114.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e119 = (_e73 + _e68);
            let _e120 = (_e78 + _e83);
            let _e121 = (_e103 + _e113);
            let _e123 = (_e118 + _e108);
            let _e128 = (-2f * _e86);
            let _e148 = (((abs(((-2f * _e78) + _e121)) + (abs((_e128 + _e119)) * 2f)) + abs(((-2f * _e83) + _e123))) >= ((abs(((-2f * _e68) + (_e108 + _e113))) + (abs((_e128 + _e120)) * 2f)) + abs(((-2f * _e73) + (_e103 + _e118)))));
            if _e148 {
                phi_208_ = _e63.y;
            } else {
                phi_208_ = _e63.x;
            }
            let _e152 = phi_208_;
            let _e153 = select(_e78, _e73, _e148);
            let _e154 = select(_e83, _e68, _e148);
            let _e157 = abs((_e153 - _e86));
            let _e158 = abs((_e154 - _e86));
            let _e161 = (0.25f * max(_e157, _e158));
            if (_e157 >= _e158) {
                phi_226_ = (0.5f * (_e153 + _e86));
                phi_227_ = -(_e152);
            } else {
                phi_226_ = (0.5f * (_e154 + _e86));
                phi_227_ = _e152;
            }
            let _e168 = phi_226_;
            let _e170 = phi_227_;
            if _e148 {
                phi_239_ = vec2<f32>(_e52.x, (_e52.y + (_e170 * 0.5f)));
            } else {
                phi_239_ = vec2<f32>((_e52.x + (_e170 * 0.5f)), _e52.y);
            }
            let _e184 = phi_239_;
            if _e148 {
                phi_247_ = vec2<f32>(_e63.x, 0f);
            } else {
                phi_247_ = vec2<f32>(0f, _e63.y);
            }
            let _e190 = phi_247_;
            let _e191 = (_e190 * 1f);
            let _e192 = (_e184 - _e191);
            let _e193 = (_e184 + _e191);
            let _e194 = textureSampleLevel(linear_image, source_sampler, _e192, 0f);
            let _e199 = textureSampleLevel(linear_image, source_sampler, _e193, 0f);
            let _e204 = (sqrt(max(dot(_e194.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e168);
            let _e205 = (sqrt(max(dot(_e199.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e168);
            let _e207 = (abs(_e204) >= _e161);
            let _e209 = (abs(_e205) >= _e161);
            phi_278_ = _e192;
            if !(_e207) {
                phi_278_ = (_e192 - _e191);
            }
            let _e214 = phi_278_;
            phi_283_ = _e193;
            if !(_e209) {
                phi_283_ = (_e193 + _e191);
            }
            let _e218 = phi_283_;
            phi_385_ = _e205;
            phi_386_ = _e204;
            phi_387_ = _e218;
            phi_388_ = _e214;
            if !(select(false, _e209, _e207)) {
                phi_288_ = _e205;
                phi_291_ = _e204;
                phi_293_ = _e218;
                phi_295_ = _e209;
                phi_297_ = _e214;
                phi_299_ = _e207;
                phi_301_ = 2i;
                loop {
                    let _e221 = phi_288_;
                    let _e223 = phi_291_;
                    let _e225 = phi_293_;
                    let _e227 = phi_295_;
                    let _e229 = phi_297_;
                    let _e231 = phi_299_;
                    let _e233 = phi_301_;
                    phi_381_ = _e221;
                    phi_382_ = _e223;
                    phi_383_ = _e225;
                    phi_384_ = _e229;
                    if (_e233 < 12i) {
                        phi_292_ = _e223;
                        if !(_e231) {
                            let _e236 = textureSampleLevel(linear_image, source_sampler, _e229, 0f);
                            phi_292_ = (sqrt(max(dot(_e236.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e168);
                        }
                        let _e243 = phi_292_;
                        local_1 = _e243;
                        phi_289_ = _e221;
                        if !(_e227) {
                            let _e245 = textureSampleLevel(linear_image, source_sampler, _e225, 0f);
                            phi_289_ = (sqrt(max(dot(_e245.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e168);
                        }
                        let _e252 = phi_289_;
                        let _e254 = (abs(_e243) >= _e161);
                        let _e256 = (abs(_e252) >= _e161);
                        local = _e252;
                        local_3 = _e256;
                        local_5 = _e254;
                        phi_298_ = _e229;
                        if !(_e254) {
                            let _e259 = f32(_e233);
                            if (_e259 < 5f) {
                                phi_353_ = 1f;
                            } else {
                                if (_e259 > 5f) {
                                    if (_e259 < 10f) {
                                        phi_351_ = 2f;
                                    } else {
                                        phi_351_ = select(8f, 4f, (_e259 < 11f));
                                    }
                                    let _e266 = phi_351_;
                                    phi_352_ = _e266;
                                } else {
                                    phi_352_ = 1.5f;
                                }
                                let _e268 = phi_352_;
                                phi_353_ = _e268;
                            }
                            let _e270 = phi_353_;
                            phi_298_ = (_e229 - (_e190 * _e270));
                        }
                        let _e274 = phi_298_;
                        local_4 = _e274;
                        phi_294_ = _e225;
                        if !(_e256) {
                            let _e276 = f32(_e233);
                            if (_e276 < 5f) {
                                phi_376_ = 1f;
                            } else {
                                if (_e276 > 5f) {
                                    if (_e276 < 10f) {
                                        phi_374_ = 2f;
                                    } else {
                                        phi_374_ = select(8f, 4f, (_e276 < 11f));
                                    }
                                    let _e283 = phi_374_;
                                    phi_375_ = _e283;
                                } else {
                                    phi_375_ = 1.5f;
                                }
                                let _e285 = phi_375_;
                                phi_376_ = _e285;
                            }
                            let _e287 = phi_376_;
                            phi_294_ = (_e225 + (_e190 * _e287));
                        }
                        let _e291 = phi_294_;
                        local_2 = _e291;
                        if select(false, _e256, _e254) {
                            phi_381_ = _e252;
                            phi_382_ = _e243;
                            phi_383_ = _e291;
                            phi_384_ = _e274;
                            break;
                        }
                        continue;
                    } else {
                        break;
                    }
                    continuing {
                        let _e388 = local;
                        phi_288_ = _e388;
                        let _e391 = local_1;
                        phi_291_ = _e391;
                        let _e394 = local_2;
                        phi_293_ = _e394;
                        let _e397 = local_3;
                        phi_295_ = _e397;
                        let _e400 = local_4;
                        phi_297_ = _e400;
                        let _e403 = local_5;
                        phi_299_ = _e403;
                        phi_301_ = (_e233 + 1i);
                    }
                }
                let _e294 = phi_381_;
                let _e296 = phi_382_;
                let _e298 = phi_383_;
                let _e300 = phi_384_;
                phi_385_ = _e294;
                phi_386_ = _e296;
                phi_387_ = _e298;
                phi_388_ = _e300;
            }
            let _e302 = phi_385_;
            let _e304 = phi_386_;
            let _e306 = phi_387_;
            let _e308 = phi_388_;
            if _e148 {
                phi_398_ = (_e52.x - _e308.x);
            } else {
                phi_398_ = (_e52.y - _e308.y);
            }
            let _e316 = phi_398_;
            if _e148 {
                phi_408_ = (_e306.x - _e52.x);
            } else {
                phi_408_ = (_e306.y - _e52.y);
            }
            let _e324 = phi_408_;
            let _e328 = (_e86 < _e168);
            let _e348 = clamp((abs(((0.083333336f * (((2f * (_e119 + _e120)) + _e121) + _e123)) - _e86)) / max(_e95, 0.00000000000000000001f)), 0f, 1f);
            let _e352 = ((((-2f * _e348) + 3f) * _e348) * _e348);
            let _e355 = max(select(0f, ((-(min(_e316, _e324)) / max((_e316 + _e324), 0.00000000000000000001f)) + 0.5f), select(((_e302 < 0f) != _e328), ((_e304 < 0f) != _e328), (_e316 < _e324))), ((_e352 * _e352) * 0.75f));
            if _e148 {
                phi_451_ = vec2<f32>(_e52.x, (_e52.y + (_e355 * _e170)));
            } else {
                phi_451_ = vec2<f32>((_e52.x + (_e355 * _e170)), _e52.y);
            }
            let _e369 = phi_451_;
            let _e370 = textureSampleLevel(linear_image, source_sampler, _e369, 0f);
            phi_457_ = _e370.xyz;
            break;
        }
    }
    let _e373 = phi_457_;
    let _e374 = clamp(_e373, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    out_u002e_var_u002e_SV_Target0_ = vec4<f32>(_e374.x, _e374.y, _e374.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

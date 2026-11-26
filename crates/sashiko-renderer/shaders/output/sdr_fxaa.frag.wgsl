@group(0) @binding(0)
var linear_image: texture_2d<f32>;
@group(0) @binding(1)
var source_sampler: sampler;
var<private> global: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    var phi_217_: f32;
    var phi_235_: f32;
    var phi_236_: f32;
    var phi_248_: vec2<f32>;
    var phi_256_: vec2<f32>;
    var phi_287_: vec2<f32>;
    var phi_292_: vec2<f32>;
    var phi_297_: f32;
    var phi_300_: f32;
    var phi_302_: vec2<f32>;
    var phi_304_: bool;
    var phi_306_: vec2<f32>;
    var phi_308_: bool;
    var phi_310_: i32;
    var phi_301_: f32;
    var phi_298_: f32;
    var phi_360_: f32;
    var phi_361_: f32;
    var phi_362_: f32;
    var phi_307_: vec2<f32>;
    var phi_383_: f32;
    var phi_384_: f32;
    var phi_385_: f32;
    var phi_303_: vec2<f32>;
    var phi_390_: f32;
    var phi_391_: f32;
    var phi_392_: vec2<f32>;
    var phi_393_: vec2<f32>;
    var phi_394_: f32;
    var phi_395_: f32;
    var phi_396_: vec2<f32>;
    var phi_397_: vec2<f32>;
    var phi_407_: f32;
    var phi_417_: f32;
    var phi_460_: vec2<f32>;
    var phi_466_: vec3<f32>;
    var local: f32;
    var local_1: f32;
    var local_2: vec2<f32>;
    var local_3: bool;
    var local_4: vec2<f32>;
    var local_5: bool;

    let _e52 = global;
    let _e54 = textureDimensions(linear_image, 0i);
    let _e60 = (_e52.xy / vec2<f32>(f32(_e54.x), f32(_e54.y)));
    let _e61 = textureSampleLevel(linear_image, source_sampler, _e60, 0f);
    let _e63 = clamp(_e61.xyz, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    switch bitcast<i32>(0u) {
        default: {
            let _e65 = textureDimensions(linear_image, 0i);
            let _e71 = (vec2<f32>(1f, 1f) / vec2<f32>(f32(_e65.x), f32(_e65.y)));
            let _e72 = textureSampleLevel(linear_image, source_sampler, _e60, 0f, vec2<i32>(0i, 1i));
            let _e76 = sqrt(max(dot(_e72.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e77 = textureSampleLevel(linear_image, source_sampler, _e60, 0f, vec2<i32>(0i, -1i));
            let _e81 = sqrt(max(dot(_e77.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e82 = textureSampleLevel(linear_image, source_sampler, _e60, 0f, vec2<i32>(-1i, 0i));
            let _e86 = sqrt(max(dot(_e82.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e87 = textureSampleLevel(linear_image, source_sampler, _e60, 0f, vec2<i32>(1i, 0i));
            let _e91 = sqrt(max(dot(_e87.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e94 = sqrt(max(dot(_e63, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e102 = max(_e94, max(max(_e76, _e81), max(_e86, _e91)));
            let _e103 = (_e102 - min(_e94, min(min(_e76, _e81), min(_e86, _e91))));
            if (_e103 < max(0.0312f, (_e102 * 0.125f))) {
                phi_466_ = _e63;
                break;
            }
            let _e107 = textureSampleLevel(linear_image, source_sampler, _e60, 0f, vec2<i32>(-1i, -1i));
            let _e111 = sqrt(max(dot(_e107.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e112 = textureSampleLevel(linear_image, source_sampler, _e60, 0f, vec2<i32>(1i, 1i));
            let _e116 = sqrt(max(dot(_e112.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e117 = textureSampleLevel(linear_image, source_sampler, _e60, 0f, vec2<i32>(-1i, 1i));
            let _e121 = sqrt(max(dot(_e117.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e122 = textureSampleLevel(linear_image, source_sampler, _e60, 0f, vec2<i32>(1i, -1i));
            let _e126 = sqrt(max(dot(_e122.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e127 = (_e81 + _e76);
            let _e128 = (_e86 + _e91);
            let _e129 = (_e111 + _e121);
            let _e131 = (_e126 + _e116);
            let _e136 = (-2f * _e94);
            let _e156 = (((abs(((-2f * _e86) + _e129)) + (abs((_e136 + _e127)) * 2f)) + abs(((-2f * _e91) + _e131))) >= ((abs(((-2f * _e76) + (_e116 + _e121))) + (abs((_e136 + _e128)) * 2f)) + abs(((-2f * _e81) + (_e111 + _e126)))));
            if _e156 {
                phi_217_ = _e71.y;
            } else {
                phi_217_ = _e71.x;
            }
            let _e160 = phi_217_;
            let _e161 = select(_e86, _e81, _e156);
            let _e162 = select(_e91, _e76, _e156);
            let _e165 = abs((_e161 - _e94));
            let _e166 = abs((_e162 - _e94));
            let _e169 = (0.25f * max(_e165, _e166));
            if (_e165 >= _e166) {
                phi_235_ = (0.5f * (_e161 + _e94));
                phi_236_ = -(_e160);
            } else {
                phi_235_ = (0.5f * (_e162 + _e94));
                phi_236_ = _e160;
            }
            let _e176 = phi_235_;
            let _e178 = phi_236_;
            if _e156 {
                phi_248_ = vec2<f32>(_e60.x, (_e60.y + (_e178 * 0.5f)));
            } else {
                phi_248_ = vec2<f32>((_e60.x + (_e178 * 0.5f)), _e60.y);
            }
            let _e192 = phi_248_;
            if _e156 {
                phi_256_ = vec2<f32>(_e71.x, 0f);
            } else {
                phi_256_ = vec2<f32>(0f, _e71.y);
            }
            let _e198 = phi_256_;
            let _e199 = (_e198 * 1f);
            let _e200 = (_e192 - _e199);
            let _e201 = (_e192 + _e199);
            let _e202 = textureSampleLevel(linear_image, source_sampler, _e200, 0f);
            let _e207 = textureSampleLevel(linear_image, source_sampler, _e201, 0f);
            let _e212 = (sqrt(max(dot(_e202.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e176);
            let _e213 = (sqrt(max(dot(_e207.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e176);
            let _e215 = (abs(_e212) >= _e169);
            let _e217 = (abs(_e213) >= _e169);
            phi_287_ = _e200;
            if !(_e215) {
                phi_287_ = (_e200 - _e199);
            }
            let _e222 = phi_287_;
            phi_292_ = _e201;
            if !(_e217) {
                phi_292_ = (_e201 + _e199);
            }
            let _e226 = phi_292_;
            phi_394_ = _e213;
            phi_395_ = _e212;
            phi_396_ = _e226;
            phi_397_ = _e222;
            if !(select(false, _e217, _e215)) {
                phi_297_ = _e213;
                phi_300_ = _e212;
                phi_302_ = _e226;
                phi_304_ = _e217;
                phi_306_ = _e222;
                phi_308_ = _e215;
                phi_310_ = 2i;
                loop {
                    let _e229 = phi_297_;
                    let _e231 = phi_300_;
                    let _e233 = phi_302_;
                    let _e235 = phi_304_;
                    let _e237 = phi_306_;
                    let _e239 = phi_308_;
                    let _e241 = phi_310_;
                    phi_390_ = _e229;
                    phi_391_ = _e231;
                    phi_392_ = _e233;
                    phi_393_ = _e237;
                    if (_e241 < 12i) {
                        phi_301_ = _e231;
                        if !(_e239) {
                            let _e244 = textureSampleLevel(linear_image, source_sampler, _e237, 0f);
                            phi_301_ = (sqrt(max(dot(_e244.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e176);
                        }
                        let _e251 = phi_301_;
                        local_1 = _e251;
                        phi_298_ = _e229;
                        if !(_e235) {
                            let _e253 = textureSampleLevel(linear_image, source_sampler, _e233, 0f);
                            phi_298_ = (sqrt(max(dot(_e253.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e176);
                        }
                        let _e260 = phi_298_;
                        let _e262 = (abs(_e251) >= _e169);
                        let _e264 = (abs(_e260) >= _e169);
                        local = _e260;
                        local_3 = _e264;
                        local_5 = _e262;
                        phi_307_ = _e237;
                        if !(_e262) {
                            let _e267 = f32(_e241);
                            if (_e267 < 5f) {
                                phi_362_ = 1f;
                            } else {
                                if (_e267 > 5f) {
                                    if (_e267 < 10f) {
                                        phi_360_ = 2f;
                                    } else {
                                        phi_360_ = select(8f, 4f, (_e267 < 11f));
                                    }
                                    let _e274 = phi_360_;
                                    phi_361_ = _e274;
                                } else {
                                    phi_361_ = 1.5f;
                                }
                                let _e276 = phi_361_;
                                phi_362_ = _e276;
                            }
                            let _e278 = phi_362_;
                            phi_307_ = (_e237 - (_e198 * _e278));
                        }
                        let _e282 = phi_307_;
                        local_4 = _e282;
                        phi_303_ = _e233;
                        if !(_e264) {
                            let _e284 = f32(_e241);
                            if (_e284 < 5f) {
                                phi_385_ = 1f;
                            } else {
                                if (_e284 > 5f) {
                                    if (_e284 < 10f) {
                                        phi_383_ = 2f;
                                    } else {
                                        phi_383_ = select(8f, 4f, (_e284 < 11f));
                                    }
                                    let _e291 = phi_383_;
                                    phi_384_ = _e291;
                                } else {
                                    phi_384_ = 1.5f;
                                }
                                let _e293 = phi_384_;
                                phi_385_ = _e293;
                            }
                            let _e295 = phi_385_;
                            phi_303_ = (_e233 + (_e198 * _e295));
                        }
                        let _e299 = phi_303_;
                        local_2 = _e299;
                        if select(false, _e264, _e262) {
                            phi_390_ = _e260;
                            phi_391_ = _e251;
                            phi_392_ = _e299;
                            phi_393_ = _e282;
                            break;
                        }
                        continue;
                    } else {
                        break;
                    }
                    continuing {
                        let _e402 = local;
                        phi_297_ = _e402;
                        let _e405 = local_1;
                        phi_300_ = _e405;
                        let _e408 = local_2;
                        phi_302_ = _e408;
                        let _e411 = local_3;
                        phi_304_ = _e411;
                        let _e414 = local_4;
                        phi_306_ = _e414;
                        let _e417 = local_5;
                        phi_308_ = _e417;
                        phi_310_ = (_e241 + 1i);
                    }
                }
                let _e302 = phi_390_;
                let _e304 = phi_391_;
                let _e306 = phi_392_;
                let _e308 = phi_393_;
                phi_394_ = _e302;
                phi_395_ = _e304;
                phi_396_ = _e306;
                phi_397_ = _e308;
            }
            let _e310 = phi_394_;
            let _e312 = phi_395_;
            let _e314 = phi_396_;
            let _e316 = phi_397_;
            if _e156 {
                phi_407_ = (_e60.x - _e316.x);
            } else {
                phi_407_ = (_e60.y - _e316.y);
            }
            let _e324 = phi_407_;
            if _e156 {
                phi_417_ = (_e314.x - _e60.x);
            } else {
                phi_417_ = (_e314.y - _e60.y);
            }
            let _e332 = phi_417_;
            let _e336 = (_e94 < _e176);
            let _e356 = clamp((abs(((0.083333336f * (((2f * (_e127 + _e128)) + _e129) + _e131)) - _e94)) / max(_e103, 0.00000000000000000001f)), 0f, 1f);
            let _e360 = ((((-2f * _e356) + 3f) * _e356) * _e356);
            let _e363 = max(select(0f, ((-(min(_e324, _e332)) / max((_e324 + _e332), 0.00000000000000000001f)) + 0.5f), select(((_e310 < 0f) != _e336), ((_e312 < 0f) != _e336), (_e324 < _e332))), ((_e360 * _e360) * 0.75f));
            if _e156 {
                phi_460_ = vec2<f32>(_e60.x, (_e60.y + (_e363 * _e178)));
            } else {
                phi_460_ = vec2<f32>((_e60.x + (_e363 * _e178)), _e60.y);
            }
            let _e377 = phi_460_;
            let _e378 = textureSampleLevel(linear_image, source_sampler, _e377, 0f);
            phi_466_ = _e378.xyz;
            break;
        }
    }
    let _e381 = phi_466_;
    let _e382 = clamp(_e381, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    let _e388 = select((_e382 * 12.92f), ((pow(_e382, vec3<f32>(0.41666666f, 0.41666666f, 0.41666666f)) * 1.055f) - vec3<f32>(0.055f, 0.055f, 0.055f)), (_e382 > vec3<f32>(0.0031308f, 0.0031308f, 0.0031308f)));
    out_u002e_var_u002e_SV_Target0_ = vec4<f32>(_e388.x, _e388.y, _e388.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

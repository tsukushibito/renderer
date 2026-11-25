@group(0) @binding(0)
var global: texture_2d<f32>;
@group(0) @binding(1)
var global_1: sampler;
var<private> global_2: vec4<f32>;
var<private> global_3: vec4<f32>;

fn function_() {
    var phi_1550_: f32;
    var phi_1571_: f32;
    var phi_1551_: f32;
    var phi_1554_: vec2<f32>;
    var phi_1552_: vec2<f32>;
    var phi_1616_: vec2<f32>;
    var phi_1637_: vec2<f32>;
    var phi_1652_: f32;
    var phi_1641_: f32;
    var phi_1636_: vec2<f32>;
    var phi_1630_: bool;
    var phi_1614_: vec2<f32>;
    var phi_1605_: bool;
    var phi_1604_: i32;
    var phi_1640_: f32;
    var phi_1650_: f32;
    var phi_1663_: f32;
    var phi_1664_: f32;
    var phi_1665_: f32;
    var phi_1705_: vec2<f32>;
    var phi_1679_: f32;
    var phi_1680_: f32;
    var phi_1681_: f32;
    var phi_1713_: vec2<f32>;
    var phi_1743_: f32;
    var phi_1731_: f32;
    var phi_1712_: vec2<f32>;
    var phi_1703_: vec2<f32>;
    var phi_1742_: f32;
    var phi_1730_: f32;
    var phi_1711_: vec2<f32>;
    var phi_1702_: vec2<f32>;
    var phi_1709_: f32;
    var phi_1714_: f32;
    var phi_1783_: vec2<f32>;
    var phi_1784_: vec3<f32>;
    var local: f32;
    var local_1: f32;
    var local_2: vec2<f32>;
    var local_3: bool;
    var local_4: vec2<f32>;
    var local_5: bool;

    let _e43 = global_2;
    let _e45 = textureDimensions(global, 0i);
    let _e48 = (_e43.xy / vec2<f32>(vec2<i32>(_e45)));
    let _e49 = textureSampleLevel(global, global_1, _e48, 0f);
    let _e51 = clamp(_e49.xyz, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    switch bitcast<i32>(0u) {
        default: {
            let _e53 = textureDimensions(global, 0i);
            let _e56 = (vec2<f32>(1f, 1f) / vec2<f32>(vec2<i32>(_e53)));
            let _e57 = textureSampleLevel(global, global_1, _e48, 0f, vec2<i32>(0i, 1i));
            let _e61 = sqrt(max(dot(_e57.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e62 = textureSampleLevel(global, global_1, _e48, 0f, vec2<i32>(0i, -1i));
            let _e66 = sqrt(max(dot(_e62.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e67 = textureSampleLevel(global, global_1, _e48, 0f, vec2<i32>(-1i, 0i));
            let _e71 = sqrt(max(dot(_e67.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e72 = textureSampleLevel(global, global_1, _e48, 0f, vec2<i32>(1i, 0i));
            let _e76 = sqrt(max(dot(_e72.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e79 = sqrt(max(dot(_e51, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e87 = max(_e79, max(max(_e61, _e66), max(_e71, _e76)));
            let _e88 = (_e87 - min(_e79, min(min(_e61, _e66), min(_e71, _e76))));
            if (_e88 < max(0.0312f, (_e87 * 0.125f))) {
                phi_1784_ = _e51;
                break;
            }
            let _e92 = textureSampleLevel(global, global_1, _e48, 0f, vec2<i32>(-1i, -1i));
            let _e96 = sqrt(max(dot(_e92.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e97 = textureSampleLevel(global, global_1, _e48, 0f, vec2<i32>(1i, 1i));
            let _e101 = sqrt(max(dot(_e97.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e102 = textureSampleLevel(global, global_1, _e48, 0f, vec2<i32>(-1i, 1i));
            let _e106 = sqrt(max(dot(_e102.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e107 = textureSampleLevel(global, global_1, _e48, 0f, vec2<i32>(1i, -1i));
            let _e111 = sqrt(max(dot(_e107.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e112 = (_e66 + _e61);
            let _e113 = (_e71 + _e76);
            let _e114 = (_e96 + _e106);
            let _e116 = (_e111 + _e101);
            let _e121 = (-2f * _e79);
            let _e141 = (((abs(((-2f * _e71) + _e114)) + (abs((_e121 + _e112)) * 2f)) + abs(((-2f * _e76) + _e116))) >= ((abs(((-2f * _e61) + (_e101 + _e106))) + (abs((_e121 + _e113)) * 2f)) + abs(((-2f * _e66) + (_e96 + _e111)))));
            if _e141 {
                phi_1550_ = _e56.y;
            } else {
                phi_1550_ = _e56.x;
            }
            let _e145 = phi_1550_;
            let _e146 = select(_e71, _e66, _e141);
            let _e147 = select(_e76, _e61, _e141);
            let _e150 = abs((_e146 - _e79));
            let _e151 = abs((_e147 - _e79));
            let _e154 = (0.25f * max(_e150, _e151));
            if (_e150 >= _e151) {
                phi_1571_ = (0.5f * (_e146 + _e79));
                phi_1551_ = -(_e145);
            } else {
                phi_1571_ = (0.5f * (_e147 + _e79));
                phi_1551_ = _e145;
            }
            let _e161 = phi_1571_;
            let _e163 = phi_1551_;
            if _e141 {
                phi_1554_ = vec2<f32>(_e48.x, (_e48.y + (_e163 * 0.5f)));
            } else {
                phi_1554_ = vec2<f32>((_e48.x + (_e163 * 0.5f)), _e48.y);
            }
            let _e177 = phi_1554_;
            if _e141 {
                phi_1552_ = vec2<f32>(_e56.x, 0f);
            } else {
                phi_1552_ = vec2<f32>(0f, _e56.y);
            }
            let _e183 = phi_1552_;
            let _e184 = (_e183 * 1f);
            let _e185 = (_e177 - _e184);
            let _e186 = (_e177 + _e184);
            let _e187 = textureSampleLevel(global, global_1, _e185, 0f);
            let _e192 = textureSampleLevel(global, global_1, _e186, 0f);
            let _e197 = (sqrt(max(dot(_e187.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e161);
            let _e198 = (sqrt(max(dot(_e192.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e161);
            let _e200 = (abs(_e197) >= _e154);
            let _e202 = (abs(_e198) >= _e154);
            phi_1616_ = _e185;
            if !(_e200) {
                phi_1616_ = (_e185 - _e184);
            }
            let _e207 = phi_1616_;
            phi_1637_ = _e186;
            if !(_e202) {
                phi_1637_ = (_e186 + _e184);
            }
            let _e211 = phi_1637_;
            phi_1742_ = _e198;
            phi_1730_ = _e197;
            phi_1711_ = _e211;
            phi_1702_ = _e207;
            if !((_e200 && _e202)) {
                phi_1652_ = _e198;
                phi_1641_ = _e197;
                phi_1636_ = _e211;
                phi_1630_ = _e202;
                phi_1614_ = _e207;
                phi_1605_ = _e200;
                phi_1604_ = 2i;
                loop {
                    let _e214 = phi_1652_;
                    let _e216 = phi_1641_;
                    let _e218 = phi_1636_;
                    let _e220 = phi_1630_;
                    let _e222 = phi_1614_;
                    let _e224 = phi_1605_;
                    let _e226 = phi_1604_;
                    phi_1743_ = _e214;
                    phi_1731_ = _e216;
                    phi_1712_ = _e218;
                    phi_1703_ = _e222;
                    if (_e226 < 12i) {
                        phi_1640_ = _e216;
                        if !(_e224) {
                            let _e229 = textureSampleLevel(global, global_1, _e222, 0f);
                            phi_1640_ = (sqrt(max(dot(_e229.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e161);
                        }
                        let _e236 = phi_1640_;
                        local_1 = _e236;
                        phi_1650_ = _e214;
                        if !(_e220) {
                            let _e238 = textureSampleLevel(global, global_1, _e218, 0f);
                            phi_1650_ = (sqrt(max(dot(_e238.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e161);
                        }
                        let _e245 = phi_1650_;
                        let _e247 = (abs(_e236) >= _e154);
                        let _e249 = (abs(_e245) >= _e154);
                        local = _e245;
                        local_3 = _e249;
                        local_5 = _e247;
                        phi_1705_ = _e222;
                        if !(_e247) {
                            let _e252 = f32(_e226);
                            if (_e252 < 5f) {
                                phi_1665_ = 1f;
                            } else {
                                if (_e252 > 5f) {
                                    if (_e252 < 10f) {
                                        phi_1663_ = 2f;
                                    } else {
                                        phi_1663_ = select(8f, 4f, (_e252 < 11f));
                                    }
                                    let _e259 = phi_1663_;
                                    phi_1664_ = _e259;
                                } else {
                                    phi_1664_ = 1.5f;
                                }
                                let _e261 = phi_1664_;
                                phi_1665_ = _e261;
                            }
                            let _e263 = phi_1665_;
                            phi_1705_ = (_e222 - (_e183 * _e263));
                        }
                        let _e267 = phi_1705_;
                        local_4 = _e267;
                        phi_1713_ = _e218;
                        if !(_e249) {
                            let _e269 = f32(_e226);
                            if (_e269 < 5f) {
                                phi_1681_ = 1f;
                            } else {
                                if (_e269 > 5f) {
                                    if (_e269 < 10f) {
                                        phi_1679_ = 2f;
                                    } else {
                                        phi_1679_ = select(8f, 4f, (_e269 < 11f));
                                    }
                                    let _e276 = phi_1679_;
                                    phi_1680_ = _e276;
                                } else {
                                    phi_1680_ = 1.5f;
                                }
                                let _e278 = phi_1680_;
                                phi_1681_ = _e278;
                            }
                            let _e280 = phi_1681_;
                            phi_1713_ = (_e218 + (_e183 * _e280));
                        }
                        let _e284 = phi_1713_;
                        local_2 = _e284;
                        if (_e247 && _e249) {
                            phi_1743_ = _e245;
                            phi_1731_ = _e236;
                            phi_1712_ = _e284;
                            phi_1703_ = _e267;
                            break;
                        }
                        continue;
                    } else {
                        break;
                    }
                    continuing {
                        let _e381 = local;
                        phi_1652_ = _e381;
                        let _e384 = local_1;
                        phi_1641_ = _e384;
                        let _e387 = local_2;
                        phi_1636_ = _e387;
                        let _e390 = local_3;
                        phi_1630_ = _e390;
                        let _e393 = local_4;
                        phi_1614_ = _e393;
                        let _e396 = local_5;
                        phi_1605_ = _e396;
                        phi_1604_ = (_e226 + 1i);
                    }
                }
                let _e287 = phi_1743_;
                let _e289 = phi_1731_;
                let _e291 = phi_1712_;
                let _e293 = phi_1703_;
                phi_1742_ = _e287;
                phi_1730_ = _e289;
                phi_1711_ = _e291;
                phi_1702_ = _e293;
            }
            let _e295 = phi_1742_;
            let _e297 = phi_1730_;
            let _e299 = phi_1711_;
            let _e301 = phi_1702_;
            if _e141 {
                phi_1709_ = (_e48.x - _e301.x);
            } else {
                phi_1709_ = (_e48.y - _e301.y);
            }
            let _e309 = phi_1709_;
            if _e141 {
                phi_1714_ = (_e299.x - _e48.x);
            } else {
                phi_1714_ = (_e299.y - _e48.y);
            }
            let _e317 = phi_1714_;
            let _e321 = (_e79 < _e161);
            let _e341 = clamp((abs(((0.083333336f * (((2f * (_e112 + _e113)) + _e114) + _e116)) - _e79)) / max(_e88, 0.00000000000000000001f)), 0f, 1f);
            let _e345 = ((((-2f * _e341) + 3f) * _e341) * _e341);
            let _e348 = max(select(0f, ((-(min(_e309, _e317)) / max((_e309 + _e317), 0.00000000000000000001f)) + 0.5f), select(((_e295 < 0f) != _e321), ((_e297 < 0f) != _e321), (_e309 < _e317))), ((_e345 * _e345) * 0.75f));
            if _e141 {
                phi_1783_ = vec2<f32>(_e48.x, (_e48.y + (_e348 * _e163)));
            } else {
                phi_1783_ = vec2<f32>((_e48.x + (_e348 * _e163)), _e48.y);
            }
            let _e362 = phi_1783_;
            let _e363 = textureSampleLevel(global, global_1, _e362, 0f);
            phi_1784_ = _e363.xyz;
            break;
        }
    }
    let _e366 = phi_1784_;
    let _e367 = clamp(_e366, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    global_3 = vec4<f32>(_e367.x, _e367.y, _e367.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global_2 = param;
    function_();
    let _e3 = global_3;
    return _e3;
}

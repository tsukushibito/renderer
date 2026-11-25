@group(0) @binding(0)
var global: texture_2d<f32>;
@group(0) @binding(1)
var global_1: sampler;
var<private> global_2: vec4<f32>;
var<private> global_3: vec4<f32>;

fn function_() {
    var phi_1569_: f32;
    var phi_1590_: f32;
    var phi_1570_: f32;
    var phi_1573_: vec2<f32>;
    var phi_1571_: vec2<f32>;
    var phi_1635_: vec2<f32>;
    var phi_1656_: vec2<f32>;
    var phi_1671_: f32;
    var phi_1660_: f32;
    var phi_1655_: vec2<f32>;
    var phi_1649_: bool;
    var phi_1633_: vec2<f32>;
    var phi_1624_: bool;
    var phi_1623_: i32;
    var phi_1659_: f32;
    var phi_1669_: f32;
    var phi_1682_: f32;
    var phi_1683_: f32;
    var phi_1684_: f32;
    var phi_1724_: vec2<f32>;
    var phi_1698_: f32;
    var phi_1699_: f32;
    var phi_1700_: f32;
    var phi_1732_: vec2<f32>;
    var phi_1762_: f32;
    var phi_1750_: f32;
    var phi_1731_: vec2<f32>;
    var phi_1722_: vec2<f32>;
    var phi_1761_: f32;
    var phi_1749_: f32;
    var phi_1730_: vec2<f32>;
    var phi_1721_: vec2<f32>;
    var phi_1728_: f32;
    var phi_1733_: f32;
    var phi_1802_: vec2<f32>;
    var phi_1803_: vec3<f32>;
    var local: f32;
    var local_1: f32;
    var local_2: vec2<f32>;
    var local_3: bool;
    var local_4: vec2<f32>;
    var local_5: bool;

    let _e51 = global_2;
    let _e53 = textureDimensions(global, 0i);
    let _e56 = (_e51.xy / vec2<f32>(vec2<i32>(_e53)));
    let _e57 = textureSampleLevel(global, global_1, _e56, 0f);
    let _e59 = clamp(_e57.xyz, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    switch bitcast<i32>(0u) {
        default: {
            let _e61 = textureDimensions(global, 0i);
            let _e64 = (vec2<f32>(1f, 1f) / vec2<f32>(vec2<i32>(_e61)));
            let _e65 = textureSampleLevel(global, global_1, _e56, 0f, vec2<i32>(0i, 1i));
            let _e69 = sqrt(max(dot(_e65.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e70 = textureSampleLevel(global, global_1, _e56, 0f, vec2<i32>(0i, -1i));
            let _e74 = sqrt(max(dot(_e70.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e75 = textureSampleLevel(global, global_1, _e56, 0f, vec2<i32>(-1i, 0i));
            let _e79 = sqrt(max(dot(_e75.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e80 = textureSampleLevel(global, global_1, _e56, 0f, vec2<i32>(1i, 0i));
            let _e84 = sqrt(max(dot(_e80.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e87 = sqrt(max(dot(_e59, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e95 = max(_e87, max(max(_e69, _e74), max(_e79, _e84)));
            let _e96 = (_e95 - min(_e87, min(min(_e69, _e74), min(_e79, _e84))));
            if (_e96 < max(0.0312f, (_e95 * 0.125f))) {
                phi_1803_ = _e59;
                break;
            }
            let _e100 = textureSampleLevel(global, global_1, _e56, 0f, vec2<i32>(-1i, -1i));
            let _e104 = sqrt(max(dot(_e100.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e105 = textureSampleLevel(global, global_1, _e56, 0f, vec2<i32>(1i, 1i));
            let _e109 = sqrt(max(dot(_e105.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e110 = textureSampleLevel(global, global_1, _e56, 0f, vec2<i32>(-1i, 1i));
            let _e114 = sqrt(max(dot(_e110.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e115 = textureSampleLevel(global, global_1, _e56, 0f, vec2<i32>(1i, -1i));
            let _e119 = sqrt(max(dot(_e115.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f));
            let _e120 = (_e74 + _e69);
            let _e121 = (_e79 + _e84);
            let _e122 = (_e104 + _e114);
            let _e124 = (_e119 + _e109);
            let _e129 = (-2f * _e87);
            let _e149 = (((abs(((-2f * _e79) + _e122)) + (abs((_e129 + _e120)) * 2f)) + abs(((-2f * _e84) + _e124))) >= ((abs(((-2f * _e69) + (_e109 + _e114))) + (abs((_e129 + _e121)) * 2f)) + abs(((-2f * _e74) + (_e104 + _e119)))));
            if _e149 {
                phi_1569_ = _e64.y;
            } else {
                phi_1569_ = _e64.x;
            }
            let _e153 = phi_1569_;
            let _e154 = select(_e79, _e74, _e149);
            let _e155 = select(_e84, _e69, _e149);
            let _e158 = abs((_e154 - _e87));
            let _e159 = abs((_e155 - _e87));
            let _e162 = (0.25f * max(_e158, _e159));
            if (_e158 >= _e159) {
                phi_1590_ = (0.5f * (_e154 + _e87));
                phi_1570_ = -(_e153);
            } else {
                phi_1590_ = (0.5f * (_e155 + _e87));
                phi_1570_ = _e153;
            }
            let _e169 = phi_1590_;
            let _e171 = phi_1570_;
            if _e149 {
                phi_1573_ = vec2<f32>(_e56.x, (_e56.y + (_e171 * 0.5f)));
            } else {
                phi_1573_ = vec2<f32>((_e56.x + (_e171 * 0.5f)), _e56.y);
            }
            let _e185 = phi_1573_;
            if _e149 {
                phi_1571_ = vec2<f32>(_e64.x, 0f);
            } else {
                phi_1571_ = vec2<f32>(0f, _e64.y);
            }
            let _e191 = phi_1571_;
            let _e192 = (_e191 * 1f);
            let _e193 = (_e185 - _e192);
            let _e194 = (_e185 + _e192);
            let _e195 = textureSampleLevel(global, global_1, _e193, 0f);
            let _e200 = textureSampleLevel(global, global_1, _e194, 0f);
            let _e205 = (sqrt(max(dot(_e195.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e169);
            let _e206 = (sqrt(max(dot(_e200.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e169);
            let _e208 = (abs(_e205) >= _e162);
            let _e210 = (abs(_e206) >= _e162);
            phi_1635_ = _e193;
            if !(_e208) {
                phi_1635_ = (_e193 - _e192);
            }
            let _e215 = phi_1635_;
            phi_1656_ = _e194;
            if !(_e210) {
                phi_1656_ = (_e194 + _e192);
            }
            let _e219 = phi_1656_;
            phi_1761_ = _e206;
            phi_1749_ = _e205;
            phi_1730_ = _e219;
            phi_1721_ = _e215;
            if !((_e208 && _e210)) {
                phi_1671_ = _e206;
                phi_1660_ = _e205;
                phi_1655_ = _e219;
                phi_1649_ = _e210;
                phi_1633_ = _e215;
                phi_1624_ = _e208;
                phi_1623_ = 2i;
                loop {
                    let _e222 = phi_1671_;
                    let _e224 = phi_1660_;
                    let _e226 = phi_1655_;
                    let _e228 = phi_1649_;
                    let _e230 = phi_1633_;
                    let _e232 = phi_1624_;
                    let _e234 = phi_1623_;
                    phi_1762_ = _e222;
                    phi_1750_ = _e224;
                    phi_1731_ = _e226;
                    phi_1722_ = _e230;
                    if (_e234 < 12i) {
                        phi_1659_ = _e224;
                        if !(_e232) {
                            let _e237 = textureSampleLevel(global, global_1, _e230, 0f);
                            phi_1659_ = (sqrt(max(dot(_e237.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e169);
                        }
                        let _e244 = phi_1659_;
                        local_1 = _e244;
                        phi_1669_ = _e222;
                        if !(_e228) {
                            let _e246 = textureSampleLevel(global, global_1, _e226, 0f);
                            phi_1669_ = (sqrt(max(dot(_e246.xyz, vec3<f32>(0.299f, 0.587f, 0.114f)), 0f)) - _e169);
                        }
                        let _e253 = phi_1669_;
                        let _e255 = (abs(_e244) >= _e162);
                        let _e257 = (abs(_e253) >= _e162);
                        local = _e253;
                        local_3 = _e257;
                        local_5 = _e255;
                        phi_1724_ = _e230;
                        if !(_e255) {
                            let _e260 = f32(_e234);
                            if (_e260 < 5f) {
                                phi_1684_ = 1f;
                            } else {
                                if (_e260 > 5f) {
                                    if (_e260 < 10f) {
                                        phi_1682_ = 2f;
                                    } else {
                                        phi_1682_ = select(8f, 4f, (_e260 < 11f));
                                    }
                                    let _e267 = phi_1682_;
                                    phi_1683_ = _e267;
                                } else {
                                    phi_1683_ = 1.5f;
                                }
                                let _e269 = phi_1683_;
                                phi_1684_ = _e269;
                            }
                            let _e271 = phi_1684_;
                            phi_1724_ = (_e230 - (_e191 * _e271));
                        }
                        let _e275 = phi_1724_;
                        local_4 = _e275;
                        phi_1732_ = _e226;
                        if !(_e257) {
                            let _e277 = f32(_e234);
                            if (_e277 < 5f) {
                                phi_1700_ = 1f;
                            } else {
                                if (_e277 > 5f) {
                                    if (_e277 < 10f) {
                                        phi_1698_ = 2f;
                                    } else {
                                        phi_1698_ = select(8f, 4f, (_e277 < 11f));
                                    }
                                    let _e284 = phi_1698_;
                                    phi_1699_ = _e284;
                                } else {
                                    phi_1699_ = 1.5f;
                                }
                                let _e286 = phi_1699_;
                                phi_1700_ = _e286;
                            }
                            let _e288 = phi_1700_;
                            phi_1732_ = (_e226 + (_e191 * _e288));
                        }
                        let _e292 = phi_1732_;
                        local_2 = _e292;
                        if (_e255 && _e257) {
                            phi_1762_ = _e253;
                            phi_1750_ = _e244;
                            phi_1731_ = _e292;
                            phi_1722_ = _e275;
                            break;
                        }
                        continue;
                    } else {
                        break;
                    }
                    continuing {
                        let _e395 = local;
                        phi_1671_ = _e395;
                        let _e398 = local_1;
                        phi_1660_ = _e398;
                        let _e401 = local_2;
                        phi_1655_ = _e401;
                        let _e404 = local_3;
                        phi_1649_ = _e404;
                        let _e407 = local_4;
                        phi_1633_ = _e407;
                        let _e410 = local_5;
                        phi_1624_ = _e410;
                        phi_1623_ = (_e234 + 1i);
                    }
                }
                let _e295 = phi_1762_;
                let _e297 = phi_1750_;
                let _e299 = phi_1731_;
                let _e301 = phi_1722_;
                phi_1761_ = _e295;
                phi_1749_ = _e297;
                phi_1730_ = _e299;
                phi_1721_ = _e301;
            }
            let _e303 = phi_1761_;
            let _e305 = phi_1749_;
            let _e307 = phi_1730_;
            let _e309 = phi_1721_;
            if _e149 {
                phi_1728_ = (_e56.x - _e309.x);
            } else {
                phi_1728_ = (_e56.y - _e309.y);
            }
            let _e317 = phi_1728_;
            if _e149 {
                phi_1733_ = (_e307.x - _e56.x);
            } else {
                phi_1733_ = (_e307.y - _e56.y);
            }
            let _e325 = phi_1733_;
            let _e329 = (_e87 < _e169);
            let _e349 = clamp((abs(((0.083333336f * (((2f * (_e120 + _e121)) + _e122) + _e124)) - _e87)) / max(_e96, 0.00000000000000000001f)), 0f, 1f);
            let _e353 = ((((-2f * _e349) + 3f) * _e349) * _e349);
            let _e356 = max(select(0f, ((-(min(_e317, _e325)) / max((_e317 + _e325), 0.00000000000000000001f)) + 0.5f), select(((_e303 < 0f) != _e329), ((_e305 < 0f) != _e329), (_e317 < _e325))), ((_e353 * _e353) * 0.75f));
            if _e149 {
                phi_1802_ = vec2<f32>(_e56.x, (_e56.y + (_e356 * _e171)));
            } else {
                phi_1802_ = vec2<f32>((_e56.x + (_e356 * _e171)), _e56.y);
            }
            let _e370 = phi_1802_;
            let _e371 = textureSampleLevel(global, global_1, _e370, 0f);
            phi_1803_ = _e371.xyz;
            break;
        }
    }
    let _e374 = phi_1803_;
    let _e375 = clamp(_e374, vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    let _e381 = select((_e375 * 12.92f), ((pow(_e375, vec3<f32>(0.41666666f, 0.41666666f, 0.41666666f)) * 1.055f) - vec3<f32>(0.055f, 0.055f, 0.055f)), (_e375 > vec3<f32>(0.0031308f, 0.0031308f, 0.0031308f)));
    global_3 = vec4<f32>(_e381.x, _e381.y, _e381.z, 1f);
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global_2 = param;
    function_();
    let _e3 = global_3;
    return _e3;
}

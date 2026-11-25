struct type_u002e_PushConstant_u002e_Selection {
    id: u32,
}

@group(0) @binding(0)
var ids: texture_2d<f32>;
@group(0) @binding(255) var<uniform> selection: type_u002e_PushConstant_u002e_Selection;
var<private> global: vec4<f32>;
var<private> out_u002e_var_u002e_SV_Target0_: vec4<f32>;

fn main_1() {
    var phi_65_: bool;
    var phi_90_: bool;
    var phi_104_: bool;
    var phi_129_: bool;
    var phi_144_: bool;
    var phi_169_: bool;
    var phi_170_: bool;
    var phi_185_: bool;
    var phi_210_: bool;
    var phi_211_: bool;
    var phi_226_: bool;
    var phi_251_: bool;
    var phi_252_: bool;
    var phi_255_: vec4<f32>;

    let _e23 = global;
    switch bitcast<i32>(0u) {
        default: {
            let _e25 = textureDimensions(ids, 0i);
            let _e27 = vec2<i32>(_e23.xy);
            switch bitcast<i32>(0u) {
                default: {
                    phi_65_ = true;
                    if !(any((_e27 < vec2<i32>(0i, 0i)))) {
                        phi_65_ = any((bitcast<vec2<u32>>(_e27) >= _e25));
                    }
                    let _e36 = phi_65_;
                    if _e36 {
                        phi_90_ = false;
                        break;
                    }
                    let _e41 = textureLoad(ids, vec3<i32>(_e27.x, _e27.y, 0i).xy, 0i);
                    let _e44 = vec4<u32>(round((_e41 * 255f)));
                    let _e59 = selection.id;
                    phi_90_ = ((((_e44.x | (_e44.y << bitcast<u32>(8u))) | (_e44.z << bitcast<u32>(16u))) | (_e44.w << bitcast<u32>(24u))) == _e59);
                    break;
                }
            }
            let _e62 = phi_90_;
            if _e62 {
                phi_255_ = vec4<f32>(0f, 0f, 0f, 0f);
                break;
            }
            let _e63 = (_e27 + vec2<i32>(-1i, 0i));
            switch bitcast<i32>(0u) {
                default: {
                    phi_104_ = true;
                    if !(any((_e63 < vec2<i32>(0i, 0i)))) {
                        phi_104_ = any((bitcast<vec2<u32>>(_e63) >= _e25));
                    }
                    let _e72 = phi_104_;
                    if _e72 {
                        phi_129_ = false;
                        break;
                    }
                    let _e77 = textureLoad(ids, vec3<i32>(_e63.x, _e63.y, 0i).xy, 0i);
                    let _e80 = vec4<u32>(round((_e77 * 255f)));
                    let _e95 = selection.id;
                    phi_129_ = ((((_e80.x | (_e80.y << bitcast<u32>(8u))) | (_e80.z << bitcast<u32>(16u))) | (_e80.w << bitcast<u32>(24u))) == _e95);
                    break;
                }
            }
            let _e98 = phi_129_;
            phi_170_ = true;
            if !(_e98) {
                let _e100 = (_e27 + vec2<i32>(1i, 0i));
                switch bitcast<i32>(0u) {
                    default: {
                        phi_144_ = true;
                        if !(any((_e100 < vec2<i32>(0i, 0i)))) {
                            phi_144_ = any((bitcast<vec2<u32>>(_e100) >= _e25));
                        }
                        let _e109 = phi_144_;
                        if _e109 {
                            phi_169_ = false;
                            break;
                        }
                        let _e114 = textureLoad(ids, vec3<i32>(_e100.x, _e100.y, 0i).xy, 0i);
                        let _e117 = vec4<u32>(round((_e114 * 255f)));
                        let _e132 = selection.id;
                        phi_169_ = ((((_e117.x | (_e117.y << bitcast<u32>(8u))) | (_e117.z << bitcast<u32>(16u))) | (_e117.w << bitcast<u32>(24u))) == _e132);
                        break;
                    }
                }
                let _e135 = phi_169_;
                phi_170_ = _e135;
            }
            let _e137 = phi_170_;
            phi_211_ = true;
            if !(_e137) {
                let _e139 = (_e27 + vec2<i32>(0i, -1i));
                switch bitcast<i32>(0u) {
                    default: {
                        phi_185_ = true;
                        if !(any((_e139 < vec2<i32>(0i, 0i)))) {
                            phi_185_ = any((bitcast<vec2<u32>>(_e139) >= _e25));
                        }
                        let _e148 = phi_185_;
                        if _e148 {
                            phi_210_ = false;
                            break;
                        }
                        let _e153 = textureLoad(ids, vec3<i32>(_e139.x, _e139.y, 0i).xy, 0i);
                        let _e156 = vec4<u32>(round((_e153 * 255f)));
                        let _e171 = selection.id;
                        phi_210_ = ((((_e156.x | (_e156.y << bitcast<u32>(8u))) | (_e156.z << bitcast<u32>(16u))) | (_e156.w << bitcast<u32>(24u))) == _e171);
                        break;
                    }
                }
                let _e174 = phi_210_;
                phi_211_ = _e174;
            }
            let _e176 = phi_211_;
            phi_252_ = true;
            if !(_e176) {
                let _e178 = (_e27 + vec2<i32>(0i, 1i));
                switch bitcast<i32>(0u) {
                    default: {
                        phi_226_ = true;
                        if !(any((_e178 < vec2<i32>(0i, 0i)))) {
                            phi_226_ = any((bitcast<vec2<u32>>(_e178) >= _e25));
                        }
                        let _e187 = phi_226_;
                        if _e187 {
                            phi_251_ = false;
                            break;
                        }
                        let _e192 = textureLoad(ids, vec3<i32>(_e178.x, _e178.y, 0i).xy, 0i);
                        let _e195 = vec4<u32>(round((_e192 * 255f)));
                        let _e210 = selection.id;
                        phi_251_ = ((((_e195.x | (_e195.y << bitcast<u32>(8u))) | (_e195.z << bitcast<u32>(16u))) | (_e195.w << bitcast<u32>(24u))) == _e210);
                        break;
                    }
                }
                let _e213 = phi_251_;
                phi_252_ = _e213;
            }
            let _e215 = phi_252_;
            phi_255_ = select(vec4<f32>(0f, 0f, 0f, 0f), vec4<f32>(1f, 0f, 1f, 1f), vec4(_e215));
            break;
        }
    }
    let _e219 = phi_255_;
    out_u002e_var_u002e_SV_Target0_ = _e219;
    return;
}

@fragment
fn main(@builtin(position) param: vec4<f32>) -> @location(0) vec4<f32> {
    global = param;
    main_1();
    let _e3 = out_u002e_var_u002e_SV_Target0_;
    return _e3;
}

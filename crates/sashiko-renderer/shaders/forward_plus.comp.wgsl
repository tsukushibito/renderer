struct type_u002e_CullCamera {
    view_projection: mat4x4<f32>,
}

@group(0) @binding(2)
var local_lights: texture_2d<f32>;
@group(0) @binding(0)
var<uniform> CullCamera: type_u002e_CullCamera;
@group(0) @binding(3)
var tile_masks: texture_storage_2d<rgba16float,write>;
var<private> global: vec3<u32>;

fn main_1() {
    var phi_75_: bool;
    var phi_80_: u32;
    var phi_86_: bool;
    var phi_91_: u32;
    var phi_173_: bool;
    var phi_178_: u32;
    var phi_184_: bool;
    var phi_189_: u32;
    var phi_197_: u32;
    var phi_200_: u32;
    var phi_240_: bool;
    var phi_260_: bool;
    var phi_280_: bool;
    var phi_300_: bool;
    var phi_320_: bool;
    var phi_340_: bool;
    var phi_198_: u32;
    var phi_347_: u32;
    var local: u32;
    var local_1: u32;
    var local_2: u32;

    let _e28 = global;
    switch bitcast<i32>(0u) {
        default: {
            let _e30 = textureDimensions(tile_masks);
            let _e32 = _e28.xy;
            if any((_e32 >= _e30)) {
                break;
            }
            let _e35 = textureLoad(local_lights, vec2<i32>(0i, 0i), 0i);
            phi_75_ = false;
            if (_e35.x >= 0f) {
                phi_75_ = (_e35.x <= 65535f);
            }
            let _e40 = phi_75_;
            if _e40 {
                phi_80_ = u32(_e35.x);
            } else {
                phi_80_ = 0u;
            }
            let _e43 = phi_80_;
            phi_86_ = false;
            if (_e35.y >= 0f) {
                phi_86_ = (_e35.y <= 65535f);
            }
            let _e48 = phi_86_;
            if _e48 {
                phi_91_ = u32(_e35.y);
            } else {
                phi_91_ = 0u;
            }
            let _e51 = phi_91_;
            let _e54 = max(vec4<u32>(_e43, _e51, u32(), u32()).xy, vec2<u32>(1u, 1u));
            let _e59 = max((_e30.x / ((_e54.x + 15u) / 16u)), 1u);
            let _e61 = global[0i];
            let _e64 = global[1i];
            let _e66 = (vec2<u32>((_e61 / _e59), _e64) * vec2<u32>(16u, 16u));
            let _e68 = ((_e61 % _e59) * 32u);
            phi_347_ = 0u;
            if all((_e66 < _e54)) {
                let _e75 = vec2<f32>(_e54);
                let _e77 = (((vec2<f32>(_e66) * 2f) / _e75) - vec2<f32>(1f, 1f));
                let _e81 = (((vec2<f32>(min((_e66 + vec2<u32>(16u, 16u)), _e54)) * 2f) / _e75) - vec2<f32>(1f, 1f));
                let _e83 = CullCamera.view_projection;
                let _e92 = vec4<f32>(_e83[0].x, _e83[1].x, _e83[2].x, _e83[3].x);
                let _e101 = vec4<f32>(_e83[0].y, _e83[1].y, _e83[2].y, _e83[3].y);
                let _e110 = vec4<f32>(_e83[0].z, _e83[1].z, _e83[2].z, _e83[3].z);
                let _e119 = vec4<f32>(_e83[0].w, _e83[1].w, _e83[2].w, _e83[3].w);
                let _e122 = (_e92 - (_e119 * _e77.x));
                let _e125 = ((_e119 * _e81.x) - _e92);
                let _e128 = (_e101 - (_e119 * _e77.y));
                let _e131 = ((_e119 * _e81.y) - _e101);
                let _e132 = (_e119 - _e110);
                let _e134 = textureDimensions(local_lights, 0i);
                let _e139 = textureLoad(local_lights, vec2<i32>(0i, 0i), 0i);
                phi_173_ = false;
                if (_e139.z >= 0f) {
                    phi_173_ = (_e139.z <= 65535f);
                }
                let _e144 = phi_173_;
                if _e144 {
                    phi_178_ = u32(_e139.z);
                } else {
                    phi_178_ = 0u;
                }
                let _e147 = phi_178_;
                phi_184_ = false;
                if (_e139.w >= 0f) {
                    phi_184_ = (_e139.w <= 65535f);
                }
                let _e152 = phi_184_;
                if _e152 {
                    phi_189_ = u32(_e139.w);
                } else {
                    phi_189_ = 0u;
                }
                let _e155 = phi_189_;
                phi_197_ = 0u;
                phi_200_ = _e68;
                loop {
                    let _e164 = phi_197_;
                    let _e166 = phi_200_;
                    local_2 = _e164;
                    if (_e166 < min((_e68 + 32u), min((_e147 | (_e155 << bitcast<u32>(16u))), (max(((_e134.x / 4u) * _e134.y), 1u) - 1u)))) {
                        let _e168 = textureDimensions(local_lights, 0i);
                        let _e171 = max((_e168.x / 4u), 1u);
                        let _e172 = (_e166 + 1u);
                        let _e180 = textureLoad(local_lights, vec3<i32>(bitcast<i32>((4u * (_e172 % _e171))), bitcast<i32>((_e172 / _e171)), 0i).xy, 0i);
                        local_1 = _e172;
                        phi_240_ = false;
                        if (_e180.w > 0f) {
                            let _e187 = (_e122 * vec4<f32>(_e180.x, _e180.y, _e180.z, 1f));
                            let _e190 = (_e180.w * length(_e122.xyz));
                            phi_240_ = (dot(_e187, vec4<f32>(1f, 1f, 1f, 1f)) >= (-(_e190) - (((dot(abs(_e187), vec4<f32>(1f, 1f, 1f, 1f)) + _e190) + 1f) * 0.00001f)));
                        }
                        let _e201 = phi_240_;
                        phi_260_ = false;
                        if _e201 {
                            let _e206 = (_e125 * vec4<f32>(_e180.x, _e180.y, _e180.z, 1f));
                            let _e209 = (_e180.w * length(_e125.xyz));
                            phi_260_ = (dot(_e206, vec4<f32>(1f, 1f, 1f, 1f)) >= (-(_e209) - (((dot(abs(_e206), vec4<f32>(1f, 1f, 1f, 1f)) + _e209) + 1f) * 0.00001f)));
                        }
                        let _e220 = phi_260_;
                        phi_280_ = false;
                        if _e220 {
                            let _e225 = (_e128 * vec4<f32>(_e180.x, _e180.y, _e180.z, 1f));
                            let _e228 = (_e180.w * length(_e128.xyz));
                            phi_280_ = (dot(_e225, vec4<f32>(1f, 1f, 1f, 1f)) >= (-(_e228) - (((dot(abs(_e225), vec4<f32>(1f, 1f, 1f, 1f)) + _e228) + 1f) * 0.00001f)));
                        }
                        let _e239 = phi_280_;
                        phi_300_ = false;
                        if _e239 {
                            let _e244 = (_e131 * vec4<f32>(_e180.x, _e180.y, _e180.z, 1f));
                            let _e247 = (_e180.w * length(_e131.xyz));
                            phi_300_ = (dot(_e244, vec4<f32>(1f, 1f, 1f, 1f)) >= (-(_e247) - (((dot(abs(_e244), vec4<f32>(1f, 1f, 1f, 1f)) + _e247) + 1f) * 0.00001f)));
                        }
                        let _e258 = phi_300_;
                        phi_320_ = false;
                        if _e258 {
                            let _e263 = (_e110 * vec4<f32>(_e180.x, _e180.y, _e180.z, 1f));
                            let _e266 = (_e180.w * length(_e110.xyz));
                            phi_320_ = (dot(_e263, vec4<f32>(1f, 1f, 1f, 1f)) >= (-(_e266) - (((dot(abs(_e263), vec4<f32>(1f, 1f, 1f, 1f)) + _e266) + 1f) * 0.00001f)));
                        }
                        let _e277 = phi_320_;
                        phi_340_ = false;
                        if _e277 {
                            let _e282 = (_e132 * vec4<f32>(_e180.x, _e180.y, _e180.z, 1f));
                            let _e285 = (_e180.w * length(_e132.xyz));
                            phi_340_ = (dot(_e282, vec4<f32>(1f, 1f, 1f, 1f)) >= (-(_e285) - (((dot(abs(_e282), vec4<f32>(1f, 1f, 1f, 1f)) + _e285) + 1f) * 0.00001f)));
                        }
                        let _e296 = phi_340_;
                        phi_198_ = _e164;
                        if _e296 {
                            phi_198_ = (_e164 | (1u << bitcast<u32>(((_e166 - _e68) & 31u))));
                        }
                        let _e303 = phi_198_;
                        local = _e303;
                        continue;
                    } else {
                        break;
                    }
                    continuing {
                        let _e330 = local;
                        phi_197_ = _e330;
                        let _e333 = local_1;
                        phi_200_ = _e333;
                    }
                }
                let _e343 = local_2;
                phi_347_ = _e343;
            }
            let _e305 = phi_347_;
            textureStore(tile_masks, _e32, vec4<f32>(f32((_e305 & 255u)), f32(((_e305 >> bitcast<u32>(8u)) & 255u)), f32(((_e305 >> bitcast<u32>(16u)) & 255u)), f32((_e305 >> bitcast<u32>(24u)))));
            break;
        }
    }
    return;
}

@compute @workgroup_size(8, 4, 1)
fn main(@builtin(global_invocation_id) param: vec3<u32>) {
    global = param;
    main_1();
}

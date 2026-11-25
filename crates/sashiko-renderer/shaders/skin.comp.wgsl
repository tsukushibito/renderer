struct type_u002e_Palette {
    rows: array<vec4<f32>, 768>,
}

struct type_u002e_ByteAddressBuffer {
    member: array<u32>,
}

struct type_u002e_RWByteAddressBuffer {
    member: array<u32>,
}

@group(0) @binding(0)
var<uniform> Palette: type_u002e_Palette;
@group(0) @binding(1)
var<storage> rest: type_u002e_ByteAddressBuffer;
@group(0) @binding(2)
var<storage, read_write> posed: type_u002e_RWByteAddressBuffer;
var<private> global: vec3<u32>;

fn main_1() {
    var local: vec2<u32>;
    var local_1: vec4<f32>;
    var phi_126_: vec3<f32>;
    var phi_129_: vec3<f32>;
    var phi_131_: vec3<f32>;
    var phi_133_: vec3<f32>;
    var phi_135_: u32;
    var phi_127_: vec3<f32>;
    var phi_130_: vec3<f32>;
    var phi_132_: vec3<f32>;
    var phi_134_: vec3<f32>;
    var local_2: vec3<f32>;
    var local_3: vec3<f32>;
    var local_4: vec3<f32>;
    var local_5: vec3<f32>;
    var local_6: vec3<f32>;
    var phi_266_: f32;
    var local_7: vec3<f32>;
    var local_8: vec3<f32>;
    var local_9: vec3<f32>;

    let _e27 = global;
    let _e29 = (_e27.x * 72u);
    let _e31 = (_e29 >> bitcast<u32>(2u));
    let _e34 = rest.member[_e31];
    let _e38 = rest.member[(_e31 + 1u)];
    let _e42 = rest.member[(_e31 + 2u)];
    let _e44 = bitcast<vec3<f32>>(vec3<u32>(_e34, _e38, _e42));
    let _e47 = ((_e29 + 12u) >> bitcast<u32>(2u));
    let _e50 = rest.member[_e47];
    let _e54 = rest.member[(_e47 + 1u)];
    let _e58 = rest.member[(_e47 + 2u)];
    let _e60 = bitcast<vec3<f32>>(vec3<u32>(_e50, _e54, _e58));
    let _e63 = ((_e29 + 24u) >> bitcast<u32>(2u));
    let _e66 = rest.member[_e63];
    let _e70 = rest.member[(_e63 + 1u)];
    let _e74 = rest.member[(_e63 + 2u)];
    let _e78 = rest.member[(_e63 + 3u)];
    let _e80 = bitcast<vec4<f32>>(vec4<u32>(_e66, _e70, _e74, _e78));
    let _e81 = _e80.xyz;
    let _e84 = (cross(_e60, _e81) * _e80.w);
    let _e87 = ((_e29 + 48u) >> bitcast<u32>(2u));
    let _e90 = rest.member[_e87];
    let _e94 = rest.member[(_e87 + 1u)];
    local = vec2<u32>(_e90, _e94);
    let _e98 = ((_e29 + 56u) >> bitcast<u32>(2u));
    let _e101 = rest.member[_e98];
    let _e105 = rest.member[(_e98 + 1u)];
    let _e109 = rest.member[(_e98 + 2u)];
    let _e113 = rest.member[(_e98 + 3u)];
    local_1 = bitcast<vec4<f32>>(vec4<u32>(_e101, _e105, _e109, _e113));
    phi_126_ = vec3<f32>(0f, 0f, 0f);
    phi_129_ = vec3<f32>(0f, 0f, 0f);
    phi_131_ = vec3<f32>(0f, 0f, 0f);
    phi_133_ = vec3<f32>(0f, 0f, 0f);
    phi_135_ = 0u;
    loop {
        let _e117 = phi_126_;
        let _e119 = phi_129_;
        let _e121 = phi_131_;
        let _e123 = phi_133_;
        let _e125 = phi_135_;
        local_2 = _e123;
        local_3 = _e121;
        local_4 = _e119;
        local_5 = _e117;
        local_6 = _e123;
        local_7 = _e121;
        local_8 = _e121;
        local_9 = _e121;
        if (_e125 < 4u) {
            let _e128 = local_1[_e125];
            if (_e128 == 0f) {
                phi_127_ = _e117;
                phi_130_ = _e119;
                phi_132_ = _e121;
                phi_134_ = _e123;
                continue;
            }
            let _e132 = local[(_e125 / 2u)];
            let _e139 = (6u * ((_e132 >> bitcast<u32>(((16u * (_e125 % 2u)) & 31u))) & 127u));
            let _e142 = Palette.rows[_e139];
            let _e146 = vec4<f32>(_e44.x, _e44.y, _e44.z, 1f);
            let _e154 = Palette.rows[(_e139 + 3u)];
            let _e160 = _e142.xyz;
            let _e172 = Palette.rows[(_e139 + 1u)];
            let _e180 = Palette.rows[(_e139 + 4u)];
            let _e186 = _e172.xyz;
            let _e198 = Palette.rows[(_e139 + 2u)];
            let _e207 = Palette.rows[(_e139 + 5u)];
            let _e214 = _e198.xyz;
            phi_127_ = vec3<f32>((_e117.x + (_e128 * dot(_e142, _e146))), (_e117.y + (_e128 * dot(_e172, _e146))), (_e117.z + (_e128 * dot(_e198, _e146))));
            phi_130_ = vec3<f32>((_e119.x + (_e128 * dot(_e160, _e84))), (_e119.y + (_e128 * dot(_e186, _e84))), (_e119.z + (_e128 * dot(_e214, _e84))));
            phi_132_ = vec3<f32>((_e121.x + (_e128 * dot(_e160, _e81))), (_e121.y + (_e128 * dot(_e186, _e81))), (_e121.z + (_e128 * dot(_e214, _e81))));
            phi_134_ = vec3<f32>((_e123.x + (_e128 * dot(_e154.xyz, _e60))), (_e123.y + (_e128 * dot(_e180.xyz, _e60))), (_e123.z + (_e128 * dot(_e207.xyz, _e60))));
            continue;
        } else {
            break;
        }
        continuing {
            let _e226 = phi_127_;
            let _e228 = phi_130_;
            let _e230 = phi_132_;
            let _e232 = phi_134_;
            phi_126_ = _e226;
            phi_129_ = _e228;
            phi_131_ = _e230;
            phi_133_ = _e232;
            phi_135_ = (_e125 + 1u);
        }
    }
    let _e235 = local_2;
    let _e237 = local_3;
    let _e240 = local_4;
    let _e241 = dot(cross(_e235, _e237), _e240);
    let _e242 = (_e27.x * 48u);
    let _e244 = (_e242 >> bitcast<u32>(2u));
    let _e246 = local_5;
    let _e247 = bitcast<vec3<u32>>(_e246);
    posed.member[_e244] = _e247.x;
    posed.member[(_e244 + 1u)] = _e247.y;
    posed.member[(_e244 + 2u)] = _e247.z;
    let _e261 = ((_e242 + 12u) >> bitcast<u32>(2u));
    let _e263 = local_6;
    let _e264 = bitcast<vec3<u32>>(_e263);
    posed.member[_e261] = _e264.x;
    posed.member[(_e261 + 1u)] = _e264.y;
    posed.member[(_e261 + 2u)] = _e264.z;
    let _e278 = ((_e242 + 24u) >> bitcast<u32>(2u));
    if (_e241 == 0f) {
        phi_266_ = _e80.w;
    } else {
        phi_266_ = f32(i32(sign(_e241)));
    }
    let _e284 = phi_266_;
    let _e286 = local_7;
    let _e289 = local_8;
    let _e292 = local_9;
    let _e295 = bitcast<vec4<u32>>(vec4<f32>(_e286.x, _e289.y, _e292.z, _e284));
    posed.member[_e278] = _e295.x;
    posed.member[(_e278 + 1u)] = _e295.y;
    posed.member[(_e278 + 2u)] = _e295.z;
    posed.member[(_e278 + 3u)] = _e295.w;
    let _e313 = ((_e242 + 40u) >> bitcast<u32>(2u));
    let _e316 = ((_e29 + 40u) >> bitcast<u32>(2u));
    let _e319 = rest.member[_e316];
    let _e323 = rest.member[(_e316 + 1u)];
    posed.member[_e313] = _e319;
    posed.member[(_e313 + 1u)] = _e323;
    return;
}

@compute @workgroup_size(64, 1, 1)
fn main(@builtin(global_invocation_id) param: vec3<u32>) {
    global = param;
    main_1();
}

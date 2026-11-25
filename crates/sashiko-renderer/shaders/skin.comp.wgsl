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
    var local: vec4<u32>;
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
    var phi_237_: f32;
    var local_5: vec3<f32>;
    var local_6: vec3<f32>;
    var local_7: vec3<f32>;
    var local_8: vec3<f32>;
    var local_9: vec3<f32>;

    let _e26 = global;
    let _e28 = (_e26.x * 72u);
    let _e30 = (_e28 >> bitcast<u32>(2u));
    let _e33 = rest.member[_e30];
    let _e37 = rest.member[(_e30 + 1u)];
    let _e41 = rest.member[(_e30 + 2u)];
    let _e43 = bitcast<vec3<f32>>(vec3<u32>(_e33, _e37, _e41));
    let _e46 = ((_e28 + 12u) >> bitcast<u32>(2u));
    let _e49 = rest.member[_e46];
    let _e53 = rest.member[(_e46 + 1u)];
    let _e57 = rest.member[(_e46 + 2u)];
    let _e59 = bitcast<vec3<f32>>(vec3<u32>(_e49, _e53, _e57));
    let _e62 = ((_e28 + 24u) >> bitcast<u32>(2u));
    let _e65 = rest.member[_e62];
    let _e69 = rest.member[(_e62 + 1u)];
    let _e73 = rest.member[(_e62 + 2u)];
    let _e77 = rest.member[(_e62 + 3u)];
    let _e79 = bitcast<vec4<f32>>(vec4<u32>(_e65, _e69, _e73, _e77));
    let _e82 = ((_e28 + 48u) >> bitcast<u32>(2u));
    let _e85 = rest.member[_e82];
    let _e89 = rest.member[(_e82 + 1u)];
    let _e92 = ((_e28 + 56u) >> bitcast<u32>(2u));
    let _e95 = rest.member[_e92];
    let _e99 = rest.member[(_e92 + 1u)];
    let _e103 = rest.member[(_e92 + 2u)];
    let _e107 = rest.member[(_e92 + 3u)];
    local = vec4<u32>(_e85, (_e85 >> bitcast<u32>(16u)), _e89, (_e89 >> bitcast<u32>(16u)));
    local_1 = bitcast<vec4<f32>>(vec4<u32>(_e95, _e99, _e103, _e107));
    let _e115 = _e79.xyz;
    let _e118 = (cross(_e59, _e115) * _e79.w);
    phi_126_ = vec3<f32>(0f, 0f, 0f);
    phi_129_ = vec3<f32>(0f, 0f, 0f);
    phi_131_ = vec3<f32>(0f, 0f, 0f);
    phi_133_ = vec3<f32>(0f, 0f, 0f);
    phi_135_ = 0u;
    loop {
        let _e120 = phi_126_;
        let _e122 = phi_129_;
        let _e124 = phi_131_;
        let _e126 = phi_133_;
        let _e128 = phi_135_;
        local_2 = _e126;
        local_3 = _e124;
        local_4 = _e122;
        local_5 = _e120;
        local_6 = _e126;
        local_7 = _e124;
        local_8 = _e124;
        local_9 = _e124;
        if (_e128 < 4u) {
            let _e131 = local_1[_e128];
            if (_e131 == 0f) {
                phi_127_ = _e120;
                phi_130_ = _e122;
                phi_132_ = _e124;
                phi_134_ = _e126;
                continue;
            }
            let _e134 = local[_e128];
            let _e136 = (6u * (_e134 & 127u));
            let _e139 = Palette.rows[_e136];
            let _e143 = vec4<f32>(_e43.x, _e43.y, _e43.z, 1f);
            let _e151 = Palette.rows[(_e136 + 3u)];
            let _e157 = _e139.xyz;
            let _e169 = Palette.rows[(_e136 + 1u)];
            let _e177 = Palette.rows[(_e136 + 4u)];
            let _e183 = _e169.xyz;
            let _e195 = Palette.rows[(_e136 + 2u)];
            let _e204 = Palette.rows[(_e136 + 5u)];
            let _e211 = _e195.xyz;
            phi_127_ = vec3<f32>((_e120.x + (_e131 * dot(_e139, _e143))), (_e120.y + (_e131 * dot(_e169, _e143))), (_e120.z + (_e131 * dot(_e195, _e143))));
            phi_130_ = vec3<f32>((_e122.x + (_e131 * dot(_e157, _e118))), (_e122.y + (_e131 * dot(_e183, _e118))), (_e122.z + (_e131 * dot(_e211, _e118))));
            phi_132_ = vec3<f32>((_e124.x + (_e131 * dot(_e157, _e115))), (_e124.y + (_e131 * dot(_e183, _e115))), (_e124.z + (_e131 * dot(_e211, _e115))));
            phi_134_ = vec3<f32>((_e126.x + (_e131 * dot(_e151.xyz, _e59))), (_e126.y + (_e131 * dot(_e177.xyz, _e59))), (_e126.z + (_e131 * dot(_e204.xyz, _e59))));
            continue;
        } else {
            break;
        }
        continuing {
            let _e223 = phi_127_;
            let _e225 = phi_130_;
            let _e227 = phi_132_;
            let _e229 = phi_134_;
            phi_126_ = _e223;
            phi_129_ = _e225;
            phi_131_ = _e227;
            phi_133_ = _e229;
            phi_135_ = (_e128 + 1u);
        }
    }
    let _e232 = local_2;
    let _e234 = local_3;
    let _e237 = local_4;
    let _e238 = dot(cross(_e232, _e234), _e237);
    if (_e238 == 0f) {
        phi_237_ = _e79.w;
    } else {
        phi_237_ = f32(i32(sign(_e238)));
    }
    let _e244 = phi_237_;
    let _e245 = (_e26.x * 48u);
    let _e247 = (_e245 >> bitcast<u32>(2u));
    let _e249 = local_5;
    let _e250 = bitcast<vec3<u32>>(_e249);
    posed.member[_e247] = _e250.x;
    posed.member[(_e247 + 1u)] = _e250.y;
    posed.member[(_e247 + 2u)] = _e250.z;
    let _e264 = ((_e245 + 12u) >> bitcast<u32>(2u));
    let _e266 = local_6;
    let _e267 = bitcast<vec3<u32>>(_e266);
    posed.member[_e264] = _e267.x;
    posed.member[(_e264 + 1u)] = _e267.y;
    posed.member[(_e264 + 2u)] = _e267.z;
    let _e281 = ((_e245 + 24u) >> bitcast<u32>(2u));
    let _e283 = local_7;
    let _e286 = local_8;
    let _e289 = local_9;
    let _e292 = bitcast<vec4<u32>>(vec4<f32>(_e283.x, _e286.y, _e289.z, _e244));
    posed.member[_e281] = _e292.x;
    posed.member[(_e281 + 1u)] = _e292.y;
    posed.member[(_e281 + 2u)] = _e292.z;
    posed.member[(_e281 + 3u)] = _e292.w;
    let _e310 = ((_e245 + 40u) >> bitcast<u32>(2u));
    let _e313 = ((_e28 + 40u) >> bitcast<u32>(2u));
    let _e316 = rest.member[_e313];
    let _e320 = rest.member[(_e313 + 1u)];
    posed.member[_e310] = _e316;
    posed.member[(_e310 + 1u)] = _e320;
    return;
}

@compute @workgroup_size(64, 1, 1)
fn main(@builtin(global_invocation_id) param: vec3<u32>) {
    global = param;
    main_1();
}

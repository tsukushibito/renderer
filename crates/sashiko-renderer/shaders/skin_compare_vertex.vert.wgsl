struct type_u002e_Palette {
    rows: array<vec4<f32>, 768>,
}

struct VertexOutput {
    @location(0) @interpolate(flat) member: vec4<f32>,
    @builtin(position) member_1: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> Palette: type_u002e_Palette;
var<private> in_u002e_var_u002e_TEXCOORD0_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD1_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD2_1: vec4<f32>;
var<private> in_u002e_var_u002e_TEXCOORD3_1: vec2<f32>;
var<private> in_u002e_var_u002e_TEXCOORD4_1: vec4<f32>;
var<private> in_u002e_var_u002e_TEXCOORD5_1: vec4<f32>;
var<private> out_u002e_var_u002e_TEXCOORD0_: vec4<f32>;
var<private> global: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    var local: vec4<u32>;
    var phi_67_: vec3<f32>;
    var phi_70_: vec3<f32>;
    var phi_72_: vec3<f32>;
    var phi_74_: vec3<f32>;
    var phi_76_: u32;
    var phi_68_: vec3<f32>;
    var phi_71_: vec3<f32>;
    var phi_73_: vec3<f32>;
    var phi_75_: vec3<f32>;
    var local_1: vec3<f32>;
    var local_2: vec3<f32>;
    var local_3: vec3<f32>;
    var phi_178_: f32;
    var local_4: vec3<f32>;
    var local_5: vec3<f32>;
    var local_6: vec3<f32>;
    var local_7: vec3<f32>;
    var local_8: vec3<f32>;

    let _e31 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e32 = in_u002e_var_u002e_TEXCOORD1_1;
    let _e33 = in_u002e_var_u002e_TEXCOORD2_1;
    let _e34 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e35 = in_u002e_var_u002e_TEXCOORD4_1;
    local = vec4<u32>(_e35);
    let _e37 = _e33.xyz;
    let _e40 = (cross(_e32, _e37) * _e33.w);
    phi_67_ = vec3<f32>(0f, 0f, 0f);
    phi_70_ = vec3<f32>(0f, 0f, 0f);
    phi_72_ = vec3<f32>(0f, 0f, 0f);
    phi_74_ = vec3<f32>(0f, 0f, 0f);
    phi_76_ = 0u;
    loop {
        let _e42 = phi_67_;
        let _e44 = phi_70_;
        let _e46 = phi_72_;
        let _e48 = phi_74_;
        let _e50 = phi_76_;
        local_1 = _e48;
        local_2 = _e46;
        local_3 = _e44;
        local_4 = _e42;
        local_5 = _e42;
        local_6 = _e42;
        local_7 = _e48;
        local_8 = _e46;
        if (_e50 < 4u) {
            let _e53 = in_u002e_var_u002e_TEXCOORD5_1[_e50];
            if (_e53 == 0f) {
                phi_68_ = _e42;
                phi_71_ = _e44;
                phi_73_ = _e46;
                phi_75_ = _e48;
                continue;
            }
            let _e56 = local[_e50];
            let _e58 = (6u * (_e56 & 127u));
            let _e61 = Palette.rows[_e58];
            let _e65 = vec4<f32>(_e31.x, _e31.y, _e31.z, 1f);
            let _e73 = Palette.rows[(_e58 + 3u)];
            let _e79 = _e61.xyz;
            let _e91 = Palette.rows[(_e58 + 1u)];
            let _e99 = Palette.rows[(_e58 + 4u)];
            let _e105 = _e91.xyz;
            let _e117 = Palette.rows[(_e58 + 2u)];
            let _e126 = Palette.rows[(_e58 + 5u)];
            let _e133 = _e117.xyz;
            phi_68_ = vec3<f32>((_e42.x + (_e53 * dot(_e61, _e65))), (_e42.y + (_e53 * dot(_e91, _e65))), (_e42.z + (_e53 * dot(_e117, _e65))));
            phi_71_ = vec3<f32>((_e44.x + (_e53 * dot(_e79, _e40))), (_e44.y + (_e53 * dot(_e105, _e40))), (_e44.z + (_e53 * dot(_e133, _e40))));
            phi_73_ = vec3<f32>((_e46.x + (_e53 * dot(_e79, _e37))), (_e46.y + (_e53 * dot(_e105, _e37))), (_e46.z + (_e53 * dot(_e133, _e37))));
            phi_75_ = vec3<f32>((_e48.x + (_e53 * dot(_e73.xyz, _e32))), (_e48.y + (_e53 * dot(_e99.xyz, _e32))), (_e48.z + (_e53 * dot(_e126.xyz, _e32))));
            continue;
        } else {
            break;
        }
        continuing {
            let _e145 = phi_68_;
            let _e147 = phi_71_;
            let _e149 = phi_73_;
            let _e151 = phi_75_;
            phi_67_ = _e145;
            phi_70_ = _e147;
            phi_72_ = _e149;
            phi_74_ = _e151;
            phi_76_ = (_e50 + 1u);
        }
    }
    let _e154 = local_1;
    let _e156 = local_2;
    let _e159 = local_3;
    let _e160 = dot(cross(_e154, _e156), _e159);
    if (_e160 == 0f) {
        phi_178_ = _e33.w;
    } else {
        phi_178_ = f32(i32(sign(_e160)));
    }
    let _e166 = phi_178_;
    let _e168 = local_4;
    let _e172 = local_5;
    let _e177 = local_6;
    let _e183 = local_7;
    let _e186 = local_8;
    let _e195 = clamp(((vec3<f32>(0.5f, 0.5f, 0.5f) + ((normalize(_e183) + (normalize(_e186) * _e166)) * 0.2f)) + (_e34.xyx * 0.1f)), vec3<f32>(0f, 0f, 0f), vec3<f32>(1f, 1f, 1f));
    out_u002e_var_u002e_TEXCOORD0_ = vec4<f32>(_e195.x, _e195.y, _e195.z, 1f);
    global = vec4<f32>((_e168.x * 0.65f), ((_e172.y - 0.9f) * 0.8f), (0.5f + (_e177.z * 0.2f)), 1f);
    return;
}

@vertex
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec3<f32>, @location(1) in_u002e_var_u002e_TEXCOORD1_: vec3<f32>, @location(2) in_u002e_var_u002e_TEXCOORD2_: vec4<f32>, @location(3) in_u002e_var_u002e_TEXCOORD3_: vec2<f32>, @location(4) in_u002e_var_u002e_TEXCOORD4_: vec4<f32>, @location(5) in_u002e_var_u002e_TEXCOORD5_: vec4<f32>) -> VertexOutput {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    in_u002e_var_u002e_TEXCOORD1_1 = in_u002e_var_u002e_TEXCOORD1_;
    in_u002e_var_u002e_TEXCOORD2_1 = in_u002e_var_u002e_TEXCOORD2_;
    in_u002e_var_u002e_TEXCOORD3_1 = in_u002e_var_u002e_TEXCOORD3_;
    in_u002e_var_u002e_TEXCOORD4_1 = in_u002e_var_u002e_TEXCOORD4_;
    in_u002e_var_u002e_TEXCOORD5_1 = in_u002e_var_u002e_TEXCOORD5_;
    main_1();
    let _e15 = global.y;
    global.y = -(_e15);
    let _e17 = out_u002e_var_u002e_TEXCOORD0_;
    let _e18 = global;
    return VertexOutput(_e17, _e18);
}

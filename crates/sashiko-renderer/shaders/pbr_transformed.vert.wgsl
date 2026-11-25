struct type_u002e_ByteAddressBuffer {
    member: array<u32>,
}

struct type_u002e_ConstantBuffer_u002e_CameraData {
    view_projection: mat4x4<f32>,
    eye: vec4<f32>,
    light_direction: vec4<f32>,
    light_color: vec4<f32>,
    environment: vec4<f32>,
    shadow_view_projection: array<mat4x4<f32>, 4>,
    shadow_splits: vec4<f32>,
    shadow_texel_world: vec4<f32>,
    shadow_depth_scale: vec4<f32>,
    camera_forward_blend: vec4<f32>,
    shadow_bias: vec4<f32>,
}

struct type_u002e_ConstantBuffer_u002e_Parameters {
    first_transform: u32,
}

struct VertexOutput {
    @location(0) member: vec3<f32>,
    @location(1) member_1: vec3<f32>,
    @location(2) member_2: vec4<f32>,
    @location(3) member_3: vec2<f32>,
    @location(4) @interpolate(flat) member_4: u32,
    @location(5) member_5: vec2<f32>,
    @location(6) @interpolate(flat) member_6: u32,
    @builtin(position) member_7: vec4<f32>,
}

@group(0) @binding(4)
var<storage> transform_table: type_u002e_ByteAddressBuffer;
@group(0) @binding(0)
var<uniform> camera: type_u002e_ConstantBuffer_u002e_CameraData;
@group(0) @binding(255) var<uniform> parameters: type_u002e_ConstantBuffer_u002e_Parameters;
var<private> in_u002e_var_u002e_TEXCOORD0_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD1_1: vec3<f32>;
var<private> in_u002e_var_u002e_TEXCOORD2_1: vec4<f32>;
var<private> in_u002e_var_u002e_TEXCOORD3_1: vec2<f32>;
var<private> global: u32;
var<private> out_u002e_var_u002e_TEXCOORD0_: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD1_: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD2_: vec4<f32>;
var<private> out_u002e_var_u002e_TEXCOORD3_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD4_: u32;
var<private> out_u002e_var_u002e_TEXCOORD5_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD6_: u32;
var<private> global_1: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    var phi_268_: bool;
    var phi_276_: bool;
    var phi_277_: bool;
    var phi_284_: i32;
    var phi_312_: vec3<f32>;
    var local: f32;
    var local_1: bool;
    var phi_333_: vec3<f32>;
    var local_2: f32;
    var local_3: bool;
    var phi_354_: vec3<f32>;
    var phi_356_: vec4<f32>;
    var phi_357_: vec4<f32>;
    var phi_358_: vec4<f32>;
    var phi_384_: f32;
    var phi_417_: bool;
    var phi_425_: bool;
    var phi_426_: bool;
    var phi_433_: i32;
    var phi_461_: vec3<f32>;
    var local_4: f32;
    var local_5: bool;
    var phi_482_: vec3<f32>;
    var local_6: f32;
    var local_7: bool;
    var phi_503_: vec3<f32>;
    var phi_505_: vec4<f32>;
    var phi_506_: vec4<f32>;
    var phi_507_: vec4<f32>;

    let _e49 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e50 = in_u002e_var_u002e_TEXCOORD1_1;
    let _e51 = in_u002e_var_u002e_TEXCOORD2_1;
    let _e52 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e53 = global;
    let _e55 = parameters.first_transform;
    let _e56 = min(_e55, 1023u);
    let _e60 = ((_e56 + min(_e53, (1023u - _e56))) * 96u);
    let _e62 = (_e60 >> bitcast<u32>(2u));
    let _e65 = transform_table.member[_e62];
    let _e69 = transform_table.member[(_e62 + 1u)];
    let _e73 = transform_table.member[(_e62 + 2u)];
    let _e77 = transform_table.member[(_e62 + 3u)];
    let _e79 = bitcast<vec4<f32>>(vec4<u32>(_e65, _e69, _e73, _e77));
    let _e82 = ((_e60 + 16u) >> bitcast<u32>(2u));
    let _e85 = transform_table.member[_e82];
    let _e89 = transform_table.member[(_e82 + 1u)];
    let _e93 = transform_table.member[(_e82 + 2u)];
    let _e97 = transform_table.member[(_e82 + 3u)];
    let _e99 = bitcast<vec4<f32>>(vec4<u32>(_e85, _e89, _e93, _e97));
    let _e102 = ((_e60 + 32u) >> bitcast<u32>(2u));
    let _e105 = transform_table.member[_e102];
    let _e109 = transform_table.member[(_e102 + 1u)];
    let _e113 = transform_table.member[(_e102 + 2u)];
    let _e117 = transform_table.member[(_e102 + 3u)];
    let _e119 = bitcast<vec4<f32>>(vec4<u32>(_e105, _e109, _e113, _e117));
    let _e122 = ((_e60 + 48u) >> bitcast<u32>(2u));
    let _e125 = transform_table.member[_e122];
    let _e129 = transform_table.member[(_e122 + 1u)];
    let _e133 = transform_table.member[(_e122 + 2u)];
    let _e137 = transform_table.member[(_e122 + 3u)];
    let _e139 = bitcast<vec4<f32>>(vec4<u32>(_e125, _e129, _e133, _e137));
    let _e142 = ((_e60 + 64u) >> bitcast<u32>(2u));
    let _e145 = transform_table.member[_e142];
    let _e149 = transform_table.member[(_e142 + 1u)];
    let _e153 = transform_table.member[(_e142 + 2u)];
    let _e157 = transform_table.member[(_e142 + 3u)];
    let _e159 = bitcast<vec4<f32>>(vec4<u32>(_e145, _e149, _e153, _e157));
    let _e162 = ((_e60 + 80u) >> bitcast<u32>(2u));
    let _e165 = transform_table.member[_e162];
    let _e169 = transform_table.member[(_e162 + 1u)];
    let _e173 = transform_table.member[(_e162 + 2u)];
    let _e177 = transform_table.member[(_e162 + 3u)];
    let _e179 = bitcast<vec4<f32>>(vec4<u32>(_e165, _e169, _e173, _e177));
    let _e204 = ((((vec3<f32>(_e79.x, _e99.x, _e119.x) * _e49.x) + (vec3<f32>(_e79.y, _e99.y, _e119.y) * _e49.y)) + (vec3<f32>(_e79.z, _e99.z, _e119.z) * _e49.z)) + vec3<f32>(_e79.w, _e99.w, _e119.w));
    let _e206 = camera.view_projection;
    let _e220 = _e139.xyz;
    let _e221 = bitcast<vec3<u32>>(_e220);
    let _e222 = (_e221 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e228 = _e159.xyz;
    let _e229 = bitcast<vec3<u32>>(_e228);
    let _e230 = (_e229 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e236 = _e179.xyz;
    let _e237 = bitcast<vec3<u32>>(_e236);
    let _e238 = (_e237 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e245 = max(max(max(_e222.x, _e222.y), _e222.z), max(max(max(_e230.x, _e230.y), _e230.z), max(max(_e238.x, _e238.y), _e238.z)));
    switch bitcast<i32>(0u) {
        default: {
            phi_268_ = true;
            if (_e245 != 0u) {
                phi_268_ = (_e245 >= 2139095040u);
            }
            let _e250 = phi_268_;
            phi_277_ = true;
            if !(_e250) {
                phi_276_ = false;
                if (_e245 >= 841731191u) {
                    phi_276_ = (_e245 <= 1287568416u);
                }
                let _e255 = phi_276_;
                phi_277_ = _e255;
            }
            let _e257 = phi_277_;
            if _e257 {
                phi_284_ = 0i;
                break;
            }
            phi_284_ = clamp((127i - bitcast<i32>((_e245 >> bitcast<u32>(23u)))), -126i, 126i);
            break;
        }
    }
    let _e264 = phi_284_;
    phi_356_ = _e139;
    phi_357_ = _e159;
    phi_358_ = _e179;
    if (_e264 != 0i) {
        switch bitcast<i32>(0u) {
            default: {
                let _e271 = bitcast<f32>((bitcast<u32>((_e264 + 127i)) << bitcast<u32>(23u)));
                let _e272 = (_e220 * _e271);
                let _e273 = (_e264 <= 22i);
                local = _e271;
                local_1 = _e273;
                local_2 = _e271;
                local_3 = _e273;
                if _e273 {
                    phi_312_ = _e272;
                    break;
                }
                phi_312_ = select(_e272, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e221 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e264 - 22i)) << bitcast<u32>(23u))))) | (_e221 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e221 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e290 = phi_312_;
        switch bitcast<i32>(0u) {
            default: {
                let _e298 = local;
                let _e299 = (_e228 * _e298);
                let _e301 = local_1;
                if _e301 {
                    phi_333_ = _e299;
                    break;
                }
                phi_333_ = select(_e299, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e229 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e264 - 22i)) << bitcast<u32>(23u))))) | (_e229 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e229 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e318 = phi_333_;
        switch bitcast<i32>(0u) {
            default: {
                let _e326 = local_2;
                let _e327 = (_e236 * _e326);
                let _e329 = local_3;
                if _e329 {
                    phi_354_ = _e327;
                    break;
                }
                phi_354_ = select(_e327, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e237 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e264 - 22i)) << bitcast<u32>(23u))))) | (_e237 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e237 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e346 = phi_354_;
        phi_356_ = vec4<f32>(_e290.x, _e290.y, _e290.z, _e139.w);
        phi_357_ = vec4<f32>(_e318.x, _e318.y, _e318.z, _e159.w);
        phi_358_ = vec4<f32>(_e346.x, _e346.y, _e346.z, _e179.w);
    }
    let _e353 = phi_356_;
    let _e355 = phi_357_;
    let _e357 = phi_358_;
    let _e380 = select(0u, 1u, (_e139.w < 0f));
    if (_e380 != 0u) {
        phi_384_ = -(_e51.w);
    } else {
        phi_384_ = _e51.w;
    }
    let _e386 = phi_384_;
    let _e387 = _e79.xyz;
    let _e388 = bitcast<vec3<u32>>(_e387);
    let _e389 = (_e388 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e395 = _e99.xyz;
    let _e396 = bitcast<vec3<u32>>(_e395);
    let _e397 = (_e396 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e403 = _e119.xyz;
    let _e404 = bitcast<vec3<u32>>(_e403);
    let _e405 = (_e404 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e412 = max(max(max(_e389.x, _e389.y), _e389.z), max(max(max(_e397.x, _e397.y), _e397.z), max(max(_e405.x, _e405.y), _e405.z)));
    switch bitcast<i32>(0u) {
        default: {
            phi_417_ = true;
            if (_e412 != 0u) {
                phi_417_ = (_e412 >= 2139095040u);
            }
            let _e417 = phi_417_;
            phi_426_ = true;
            if !(_e417) {
                phi_425_ = false;
                if (_e412 >= 841731191u) {
                    phi_425_ = (_e412 <= 1287568416u);
                }
                let _e422 = phi_425_;
                phi_426_ = _e422;
            }
            let _e424 = phi_426_;
            if _e424 {
                phi_433_ = 0i;
                break;
            }
            phi_433_ = clamp((127i - bitcast<i32>((_e412 >> bitcast<u32>(23u)))), -126i, 126i);
            break;
        }
    }
    let _e431 = phi_433_;
    phi_505_ = _e79;
    phi_506_ = _e99;
    phi_507_ = _e119;
    if (_e431 != 0i) {
        switch bitcast<i32>(0u) {
            default: {
                let _e438 = bitcast<f32>((bitcast<u32>((_e431 + 127i)) << bitcast<u32>(23u)));
                let _e439 = (_e387 * _e438);
                let _e440 = (_e431 <= 22i);
                local_4 = _e438;
                local_5 = _e440;
                local_6 = _e438;
                local_7 = _e440;
                if _e440 {
                    phi_461_ = _e439;
                    break;
                }
                phi_461_ = select(_e439, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e388 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e431 - 22i)) << bitcast<u32>(23u))))) | (_e388 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e388 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e457 = phi_461_;
        switch bitcast<i32>(0u) {
            default: {
                let _e465 = local_4;
                let _e466 = (_e395 * _e465);
                let _e468 = local_5;
                if _e468 {
                    phi_482_ = _e466;
                    break;
                }
                phi_482_ = select(_e466, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e396 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e431 - 22i)) << bitcast<u32>(23u))))) | (_e396 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e396 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e485 = phi_482_;
        switch bitcast<i32>(0u) {
            default: {
                let _e493 = local_6;
                let _e494 = (_e403 * _e493);
                let _e496 = local_7;
                if _e496 {
                    phi_503_ = _e494;
                    break;
                }
                phi_503_ = select(_e494, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e404 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e431 - 22i)) << bitcast<u32>(23u))))) | (_e404 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e404 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e513 = phi_503_;
        phi_505_ = vec4<f32>(_e457.x, _e457.y, _e457.z, _e79.w);
        phi_506_ = vec4<f32>(_e485.x, _e485.y, _e485.z, _e99.w);
        phi_507_ = vec4<f32>(_e513.x, _e513.y, _e513.z, _e119.w);
    }
    let _e520 = phi_505_;
    let _e522 = phi_506_;
    let _e524 = phi_507_;
    let _e544 = (((vec3<f32>(_e520.x, _e522.x, _e524.x) * _e51.x) + (vec3<f32>(_e520.y, _e522.y, _e524.y) * _e51.y)) + (vec3<f32>(_e520.z, _e522.z, _e524.z) * _e51.z));
    out_u002e_var_u002e_TEXCOORD0_ = _e204;
    out_u002e_var_u002e_TEXCOORD1_ = (((vec3<f32>(_e353.x, _e355.x, _e357.x) * _e50.x) + (vec3<f32>(_e353.y, _e355.y, _e357.y) * _e50.y)) + (vec3<f32>(_e353.z, _e355.z, _e357.z) * _e50.z));
    out_u002e_var_u002e_TEXCOORD2_ = vec4<f32>(_e544.x, _e544.y, _e544.z, _e386);
    out_u002e_var_u002e_TEXCOORD3_ = _e52;
    out_u002e_var_u002e_TEXCOORD4_ = _e380;
    out_u002e_var_u002e_TEXCOORD5_ = vec2<f32>(0f, 0f);
    out_u002e_var_u002e_TEXCOORD6_ = 0u;
    global_1 = ((((_e206[0] * _e204.x) + (_e206[1] * _e204.y)) + (_e206[2] * _e204.z)) + _e206[3]);
    return;
}

@vertex
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec3<f32>, @location(1) in_u002e_var_u002e_TEXCOORD1_: vec3<f32>, @location(2) in_u002e_var_u002e_TEXCOORD2_: vec4<f32>, @location(3) in_u002e_var_u002e_TEXCOORD3_: vec2<f32>, @builtin(instance_index) param: u32) -> VertexOutput {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    in_u002e_var_u002e_TEXCOORD1_1 = in_u002e_var_u002e_TEXCOORD1_;
    in_u002e_var_u002e_TEXCOORD2_1 = in_u002e_var_u002e_TEXCOORD2_;
    in_u002e_var_u002e_TEXCOORD3_1 = in_u002e_var_u002e_TEXCOORD3_;
    global = param;
    main_1();
    let _e19 = global_1.y;
    global_1.y = -(_e19);
    let _e21 = out_u002e_var_u002e_TEXCOORD0_;
    let _e22 = out_u002e_var_u002e_TEXCOORD1_;
    let _e23 = out_u002e_var_u002e_TEXCOORD2_;
    let _e24 = out_u002e_var_u002e_TEXCOORD3_;
    let _e25 = out_u002e_var_u002e_TEXCOORD4_;
    let _e26 = out_u002e_var_u002e_TEXCOORD5_;
    let _e27 = out_u002e_var_u002e_TEXCOORD6_;
    let _e28 = global_1;
    return VertexOutput(_e21, _e22, _e23, _e24, _e25, _e26, _e27, _e28);
}

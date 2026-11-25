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
    slot: u32,
}

struct VertexOutput {
    @location(1) member: vec3<f32>,
    @location(2) member_1: vec4<f32>,
    @location(3) member_2: vec2<f32>,
    @location(4) @interpolate(flat) member_3: u32,
    @location(5) member_4: vec2<f32>,
    @location(6) @interpolate(flat) member_5: u32,
    @builtin(position) member_6: vec4<f32>,
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
var<private> in_u002e_var_u002e_TEXCOORD4_1: vec3<f32>;
var<private> global: u32;
var<private> out_u002e_var_u002e_TEXCOORD1_: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD2_: vec4<f32>;
var<private> out_u002e_var_u002e_TEXCOORD3_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD4_: u32;
var<private> out_u002e_var_u002e_TEXCOORD5_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD6_: u32;
var<private> global_1: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    var phi_269_: bool;
    var phi_277_: bool;
    var phi_278_: bool;
    var phi_285_: i32;
    var phi_313_: vec3<f32>;
    var local: f32;
    var local_1: bool;
    var phi_334_: vec3<f32>;
    var local_2: f32;
    var local_3: bool;
    var phi_355_: vec3<f32>;
    var phi_357_: vec4<f32>;
    var phi_358_: vec4<f32>;
    var phi_359_: vec4<f32>;
    var phi_385_: f32;
    var phi_418_: bool;
    var phi_426_: bool;
    var phi_427_: bool;
    var phi_434_: i32;
    var phi_462_: vec3<f32>;
    var local_4: f32;
    var local_5: bool;
    var phi_483_: vec3<f32>;
    var local_6: f32;
    var local_7: bool;
    var phi_504_: vec3<f32>;
    var phi_506_: vec4<f32>;
    var phi_507_: vec4<f32>;
    var phi_508_: vec4<f32>;

    let _e49 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e50 = in_u002e_var_u002e_TEXCOORD1_1;
    let _e51 = in_u002e_var_u002e_TEXCOORD2_1;
    let _e52 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e53 = in_u002e_var_u002e_TEXCOORD4_1;
    let _e54 = global;
    let _e56 = parameters.first_transform;
    let _e57 = min(_e56, 1023u);
    let _e61 = ((_e57 + min(_e54, (1023u - _e57))) * 96u);
    let _e63 = (_e61 >> bitcast<u32>(2u));
    let _e66 = transform_table.member[_e63];
    let _e70 = transform_table.member[(_e63 + 1u)];
    let _e74 = transform_table.member[(_e63 + 2u)];
    let _e78 = transform_table.member[(_e63 + 3u)];
    let _e80 = bitcast<vec4<f32>>(vec4<u32>(_e66, _e70, _e74, _e78));
    let _e83 = ((_e61 + 16u) >> bitcast<u32>(2u));
    let _e86 = transform_table.member[_e83];
    let _e90 = transform_table.member[(_e83 + 1u)];
    let _e94 = transform_table.member[(_e83 + 2u)];
    let _e98 = transform_table.member[(_e83 + 3u)];
    let _e100 = bitcast<vec4<f32>>(vec4<u32>(_e86, _e90, _e94, _e98));
    let _e103 = ((_e61 + 32u) >> bitcast<u32>(2u));
    let _e106 = transform_table.member[_e103];
    let _e110 = transform_table.member[(_e103 + 1u)];
    let _e114 = transform_table.member[(_e103 + 2u)];
    let _e118 = transform_table.member[(_e103 + 3u)];
    let _e120 = bitcast<vec4<f32>>(vec4<u32>(_e106, _e110, _e114, _e118));
    let _e123 = ((_e61 + 48u) >> bitcast<u32>(2u));
    let _e126 = transform_table.member[_e123];
    let _e130 = transform_table.member[(_e123 + 1u)];
    let _e134 = transform_table.member[(_e123 + 2u)];
    let _e138 = transform_table.member[(_e123 + 3u)];
    let _e140 = bitcast<vec4<f32>>(vec4<u32>(_e126, _e130, _e134, _e138));
    let _e143 = ((_e61 + 64u) >> bitcast<u32>(2u));
    let _e146 = transform_table.member[_e143];
    let _e150 = transform_table.member[(_e143 + 1u)];
    let _e154 = transform_table.member[(_e143 + 2u)];
    let _e158 = transform_table.member[(_e143 + 3u)];
    let _e160 = bitcast<vec4<f32>>(vec4<u32>(_e146, _e150, _e154, _e158));
    let _e163 = ((_e61 + 80u) >> bitcast<u32>(2u));
    let _e166 = transform_table.member[_e163];
    let _e170 = transform_table.member[(_e163 + 1u)];
    let _e174 = transform_table.member[(_e163 + 2u)];
    let _e178 = transform_table.member[(_e163 + 3u)];
    let _e180 = bitcast<vec4<f32>>(vec4<u32>(_e166, _e170, _e174, _e178));
    let _e205 = ((((vec3<f32>(_e80.x, _e100.x, _e120.x) * _e49.x) + (vec3<f32>(_e80.y, _e100.y, _e120.y) * _e49.y)) + (vec3<f32>(_e80.z, _e100.z, _e120.z) * _e49.z)) + vec3<f32>(_e80.w, _e100.w, _e120.w));
    let _e207 = camera.view_projection;
    let _e221 = _e140.xyz;
    let _e222 = bitcast<vec3<u32>>(_e221);
    let _e223 = (_e222 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e229 = _e160.xyz;
    let _e230 = bitcast<vec3<u32>>(_e229);
    let _e231 = (_e230 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e237 = _e180.xyz;
    let _e238 = bitcast<vec3<u32>>(_e237);
    let _e239 = (_e238 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e246 = max(max(max(_e223.x, _e223.y), _e223.z), max(max(max(_e231.x, _e231.y), _e231.z), max(max(_e239.x, _e239.y), _e239.z)));
    switch bitcast<i32>(0u) {
        default: {
            phi_269_ = true;
            if (_e246 != 0u) {
                phi_269_ = (_e246 >= 2139095040u);
            }
            let _e251 = phi_269_;
            phi_278_ = true;
            if !(_e251) {
                phi_277_ = false;
                if (_e246 >= 841731191u) {
                    phi_277_ = (_e246 <= 1287568416u);
                }
                let _e256 = phi_277_;
                phi_278_ = _e256;
            }
            let _e258 = phi_278_;
            if _e258 {
                phi_285_ = 0i;
                break;
            }
            phi_285_ = clamp((127i - bitcast<i32>((_e246 >> bitcast<u32>(23u)))), -126i, 126i);
            break;
        }
    }
    let _e265 = phi_285_;
    phi_357_ = _e140;
    phi_358_ = _e160;
    phi_359_ = _e180;
    if (_e265 != 0i) {
        switch bitcast<i32>(0u) {
            default: {
                let _e272 = bitcast<f32>((bitcast<u32>((_e265 + 127i)) << bitcast<u32>(23u)));
                let _e273 = (_e221 * _e272);
                let _e274 = (_e265 <= 22i);
                local = _e272;
                local_1 = _e274;
                local_2 = _e272;
                local_3 = _e274;
                if _e274 {
                    phi_313_ = _e273;
                    break;
                }
                phi_313_ = select(_e273, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e222 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e265 - 22i)) << bitcast<u32>(23u))))) | (_e222 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e222 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e291 = phi_313_;
        switch bitcast<i32>(0u) {
            default: {
                let _e299 = local;
                let _e300 = (_e229 * _e299);
                let _e302 = local_1;
                if _e302 {
                    phi_334_ = _e300;
                    break;
                }
                phi_334_ = select(_e300, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e230 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e265 - 22i)) << bitcast<u32>(23u))))) | (_e230 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e230 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e319 = phi_334_;
        switch bitcast<i32>(0u) {
            default: {
                let _e327 = local_2;
                let _e328 = (_e237 * _e327);
                let _e330 = local_3;
                if _e330 {
                    phi_355_ = _e328;
                    break;
                }
                phi_355_ = select(_e328, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e238 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e265 - 22i)) << bitcast<u32>(23u))))) | (_e238 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e238 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e347 = phi_355_;
        phi_357_ = vec4<f32>(_e291.x, _e291.y, _e291.z, _e140.w);
        phi_358_ = vec4<f32>(_e319.x, _e319.y, _e319.z, _e160.w);
        phi_359_ = vec4<f32>(_e347.x, _e347.y, _e347.z, _e180.w);
    }
    let _e354 = phi_357_;
    let _e356 = phi_358_;
    let _e358 = phi_359_;
    let _e381 = select(0u, 1u, (_e140.w < 0f));
    if (_e381 != 0u) {
        phi_385_ = -(_e51.w);
    } else {
        phi_385_ = _e51.w;
    }
    let _e387 = phi_385_;
    let _e388 = _e80.xyz;
    let _e389 = bitcast<vec3<u32>>(_e388);
    let _e390 = (_e389 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e396 = _e100.xyz;
    let _e397 = bitcast<vec3<u32>>(_e396);
    let _e398 = (_e397 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e404 = _e120.xyz;
    let _e405 = bitcast<vec3<u32>>(_e404);
    let _e406 = (_e405 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e413 = max(max(max(_e390.x, _e390.y), _e390.z), max(max(max(_e398.x, _e398.y), _e398.z), max(max(_e406.x, _e406.y), _e406.z)));
    switch bitcast<i32>(0u) {
        default: {
            phi_418_ = true;
            if (_e413 != 0u) {
                phi_418_ = (_e413 >= 2139095040u);
            }
            let _e418 = phi_418_;
            phi_427_ = true;
            if !(_e418) {
                phi_426_ = false;
                if (_e413 >= 841731191u) {
                    phi_426_ = (_e413 <= 1287568416u);
                }
                let _e423 = phi_426_;
                phi_427_ = _e423;
            }
            let _e425 = phi_427_;
            if _e425 {
                phi_434_ = 0i;
                break;
            }
            phi_434_ = clamp((127i - bitcast<i32>((_e413 >> bitcast<u32>(23u)))), -126i, 126i);
            break;
        }
    }
    let _e432 = phi_434_;
    phi_506_ = _e80;
    phi_507_ = _e100;
    phi_508_ = _e120;
    if (_e432 != 0i) {
        switch bitcast<i32>(0u) {
            default: {
                let _e439 = bitcast<f32>((bitcast<u32>((_e432 + 127i)) << bitcast<u32>(23u)));
                let _e440 = (_e388 * _e439);
                let _e441 = (_e432 <= 22i);
                local_4 = _e439;
                local_5 = _e441;
                local_6 = _e439;
                local_7 = _e441;
                if _e441 {
                    phi_462_ = _e440;
                    break;
                }
                phi_462_ = select(_e440, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e389 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e432 - 22i)) << bitcast<u32>(23u))))) | (_e389 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e389 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e458 = phi_462_;
        switch bitcast<i32>(0u) {
            default: {
                let _e466 = local_4;
                let _e467 = (_e396 * _e466);
                let _e469 = local_5;
                if _e469 {
                    phi_483_ = _e467;
                    break;
                }
                phi_483_ = select(_e467, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e397 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e432 - 22i)) << bitcast<u32>(23u))))) | (_e397 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e397 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e486 = phi_483_;
        switch bitcast<i32>(0u) {
            default: {
                let _e494 = local_6;
                let _e495 = (_e404 * _e494);
                let _e497 = local_7;
                if _e497 {
                    phi_504_ = _e495;
                    break;
                }
                phi_504_ = select(_e495, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e405 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e432 - 22i)) << bitcast<u32>(23u))))) | (_e405 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e405 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e514 = phi_504_;
        phi_506_ = vec4<f32>(_e458.x, _e458.y, _e458.z, _e80.w);
        phi_507_ = vec4<f32>(_e486.x, _e486.y, _e486.z, _e100.w);
        phi_508_ = vec4<f32>(_e514.x, _e514.y, _e514.z, _e120.w);
    }
    let _e521 = phi_506_;
    let _e523 = phi_507_;
    let _e525 = phi_508_;
    let _e545 = (((vec3<f32>(_e521.x, _e523.x, _e525.x) * _e51.x) + (vec3<f32>(_e521.y, _e523.y, _e525.y) * _e51.y)) + (vec3<f32>(_e521.z, _e523.z, _e525.z) * _e51.z));
    let _e552 = parameters.slot;
    out_u002e_var_u002e_TEXCOORD1_ = (((vec3<f32>(_e354.x, _e356.x, _e358.x) * _e50.x) + (vec3<f32>(_e354.y, _e356.y, _e358.y) * _e50.y)) + (vec3<f32>(_e354.z, _e356.z, _e358.z) * _e50.z));
    out_u002e_var_u002e_TEXCOORD2_ = vec4<f32>(_e545.x, _e545.y, _e545.z, _e387);
    out_u002e_var_u002e_TEXCOORD3_ = _e52;
    out_u002e_var_u002e_TEXCOORD4_ = _e381;
    out_u002e_var_u002e_TEXCOORD5_ = _e53.xy;
    out_u002e_var_u002e_TEXCOORD6_ = (_e552 + u32(_e53.z));
    global_1 = ((((_e207[0] * _e205.x) + (_e207[1] * _e205.y)) + (_e207[2] * _e205.z)) + _e207[3]);
    return;
}

@vertex
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec3<f32>, @location(1) in_u002e_var_u002e_TEXCOORD1_: vec3<f32>, @location(2) in_u002e_var_u002e_TEXCOORD2_: vec4<f32>, @location(3) in_u002e_var_u002e_TEXCOORD3_: vec2<f32>, @location(4) in_u002e_var_u002e_TEXCOORD4_: vec3<f32>, @builtin(instance_index) param: u32) -> VertexOutput {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    in_u002e_var_u002e_TEXCOORD1_1 = in_u002e_var_u002e_TEXCOORD1_;
    in_u002e_var_u002e_TEXCOORD2_1 = in_u002e_var_u002e_TEXCOORD2_;
    in_u002e_var_u002e_TEXCOORD3_1 = in_u002e_var_u002e_TEXCOORD3_;
    in_u002e_var_u002e_TEXCOORD4_1 = in_u002e_var_u002e_TEXCOORD4_;
    global = param;
    main_1();
    let _e20 = global_1.y;
    global_1.y = -(_e20);
    let _e22 = out_u002e_var_u002e_TEXCOORD1_;
    let _e23 = out_u002e_var_u002e_TEXCOORD2_;
    let _e24 = out_u002e_var_u002e_TEXCOORD3_;
    let _e25 = out_u002e_var_u002e_TEXCOORD4_;
    let _e26 = out_u002e_var_u002e_TEXCOORD5_;
    let _e27 = out_u002e_var_u002e_TEXCOORD6_;
    let _e28 = global_1;
    return VertexOutput(_e22, _e23, _e24, _e25, _e26, _e27, _e28);
}

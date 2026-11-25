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
var<private> global: u32;
var<private> out_u002e_var_u002e_TEXCOORD1_: vec3<f32>;
var<private> out_u002e_var_u002e_TEXCOORD2_: vec4<f32>;
var<private> out_u002e_var_u002e_TEXCOORD3_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD4_: u32;
var<private> out_u002e_var_u002e_TEXCOORD5_: vec2<f32>;
var<private> out_u002e_var_u002e_TEXCOORD6_: u32;
var<private> global_1: vec4<f32> = vec4<f32>(0f, 0f, 0f, 1f);

fn main_1() {
    var phi_267_: bool;
    var phi_275_: bool;
    var phi_276_: bool;
    var phi_283_: i32;
    var phi_311_: vec3<f32>;
    var local: f32;
    var local_1: bool;
    var phi_332_: vec3<f32>;
    var local_2: f32;
    var local_3: bool;
    var phi_353_: vec3<f32>;
    var phi_355_: vec4<f32>;
    var phi_356_: vec4<f32>;
    var phi_357_: vec4<f32>;
    var phi_383_: f32;
    var phi_416_: bool;
    var phi_424_: bool;
    var phi_425_: bool;
    var phi_432_: i32;
    var phi_460_: vec3<f32>;
    var local_4: f32;
    var local_5: bool;
    var phi_481_: vec3<f32>;
    var local_6: f32;
    var local_7: bool;
    var phi_502_: vec3<f32>;
    var phi_504_: vec4<f32>;
    var phi_505_: vec4<f32>;
    var phi_506_: vec4<f32>;

    let _e48 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e49 = in_u002e_var_u002e_TEXCOORD1_1;
    let _e50 = in_u002e_var_u002e_TEXCOORD2_1;
    let _e51 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e52 = global;
    let _e54 = parameters.first_transform;
    let _e55 = min(_e54, 1023u);
    let _e59 = ((_e55 + min(_e52, (1023u - _e55))) * 96u);
    let _e61 = (_e59 >> bitcast<u32>(2u));
    let _e64 = transform_table.member[_e61];
    let _e68 = transform_table.member[(_e61 + 1u)];
    let _e72 = transform_table.member[(_e61 + 2u)];
    let _e76 = transform_table.member[(_e61 + 3u)];
    let _e78 = bitcast<vec4<f32>>(vec4<u32>(_e64, _e68, _e72, _e76));
    let _e81 = ((_e59 + 16u) >> bitcast<u32>(2u));
    let _e84 = transform_table.member[_e81];
    let _e88 = transform_table.member[(_e81 + 1u)];
    let _e92 = transform_table.member[(_e81 + 2u)];
    let _e96 = transform_table.member[(_e81 + 3u)];
    let _e98 = bitcast<vec4<f32>>(vec4<u32>(_e84, _e88, _e92, _e96));
    let _e101 = ((_e59 + 32u) >> bitcast<u32>(2u));
    let _e104 = transform_table.member[_e101];
    let _e108 = transform_table.member[(_e101 + 1u)];
    let _e112 = transform_table.member[(_e101 + 2u)];
    let _e116 = transform_table.member[(_e101 + 3u)];
    let _e118 = bitcast<vec4<f32>>(vec4<u32>(_e104, _e108, _e112, _e116));
    let _e121 = ((_e59 + 48u) >> bitcast<u32>(2u));
    let _e124 = transform_table.member[_e121];
    let _e128 = transform_table.member[(_e121 + 1u)];
    let _e132 = transform_table.member[(_e121 + 2u)];
    let _e136 = transform_table.member[(_e121 + 3u)];
    let _e138 = bitcast<vec4<f32>>(vec4<u32>(_e124, _e128, _e132, _e136));
    let _e141 = ((_e59 + 64u) >> bitcast<u32>(2u));
    let _e144 = transform_table.member[_e141];
    let _e148 = transform_table.member[(_e141 + 1u)];
    let _e152 = transform_table.member[(_e141 + 2u)];
    let _e156 = transform_table.member[(_e141 + 3u)];
    let _e158 = bitcast<vec4<f32>>(vec4<u32>(_e144, _e148, _e152, _e156));
    let _e161 = ((_e59 + 80u) >> bitcast<u32>(2u));
    let _e164 = transform_table.member[_e161];
    let _e168 = transform_table.member[(_e161 + 1u)];
    let _e172 = transform_table.member[(_e161 + 2u)];
    let _e176 = transform_table.member[(_e161 + 3u)];
    let _e178 = bitcast<vec4<f32>>(vec4<u32>(_e164, _e168, _e172, _e176));
    let _e203 = ((((vec3<f32>(_e78.x, _e98.x, _e118.x) * _e48.x) + (vec3<f32>(_e78.y, _e98.y, _e118.y) * _e48.y)) + (vec3<f32>(_e78.z, _e98.z, _e118.z) * _e48.z)) + vec3<f32>(_e78.w, _e98.w, _e118.w));
    let _e205 = camera.view_projection;
    let _e219 = _e138.xyz;
    let _e220 = bitcast<vec3<u32>>(_e219);
    let _e221 = (_e220 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e227 = _e158.xyz;
    let _e228 = bitcast<vec3<u32>>(_e227);
    let _e229 = (_e228 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e235 = _e178.xyz;
    let _e236 = bitcast<vec3<u32>>(_e235);
    let _e237 = (_e236 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e244 = max(max(max(_e221.x, _e221.y), _e221.z), max(max(max(_e229.x, _e229.y), _e229.z), max(max(_e237.x, _e237.y), _e237.z)));
    switch bitcast<i32>(0u) {
        default: {
            phi_267_ = true;
            if (_e244 != 0u) {
                phi_267_ = (_e244 >= 2139095040u);
            }
            let _e249 = phi_267_;
            phi_276_ = true;
            if !(_e249) {
                phi_275_ = false;
                if (_e244 >= 841731191u) {
                    phi_275_ = (_e244 <= 1287568416u);
                }
                let _e254 = phi_275_;
                phi_276_ = _e254;
            }
            let _e256 = phi_276_;
            if _e256 {
                phi_283_ = 0i;
                break;
            }
            phi_283_ = clamp((127i - bitcast<i32>((_e244 >> bitcast<u32>(23u)))), -126i, 126i);
            break;
        }
    }
    let _e263 = phi_283_;
    phi_355_ = _e138;
    phi_356_ = _e158;
    phi_357_ = _e178;
    if (_e263 != 0i) {
        switch bitcast<i32>(0u) {
            default: {
                let _e270 = bitcast<f32>((bitcast<u32>((_e263 + 127i)) << bitcast<u32>(23u)));
                let _e271 = (_e219 * _e270);
                let _e272 = (_e263 <= 22i);
                local = _e270;
                local_1 = _e272;
                local_2 = _e270;
                local_3 = _e272;
                if _e272 {
                    phi_311_ = _e271;
                    break;
                }
                phi_311_ = select(_e271, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e220 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e263 - 22i)) << bitcast<u32>(23u))))) | (_e220 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e220 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e289 = phi_311_;
        switch bitcast<i32>(0u) {
            default: {
                let _e297 = local;
                let _e298 = (_e227 * _e297);
                let _e300 = local_1;
                if _e300 {
                    phi_332_ = _e298;
                    break;
                }
                phi_332_ = select(_e298, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e228 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e263 - 22i)) << bitcast<u32>(23u))))) | (_e228 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e228 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e317 = phi_332_;
        switch bitcast<i32>(0u) {
            default: {
                let _e325 = local_2;
                let _e326 = (_e235 * _e325);
                let _e328 = local_3;
                if _e328 {
                    phi_353_ = _e326;
                    break;
                }
                phi_353_ = select(_e326, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e236 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e263 - 22i)) << bitcast<u32>(23u))))) | (_e236 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e236 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e345 = phi_353_;
        phi_355_ = vec4<f32>(_e289.x, _e289.y, _e289.z, _e138.w);
        phi_356_ = vec4<f32>(_e317.x, _e317.y, _e317.z, _e158.w);
        phi_357_ = vec4<f32>(_e345.x, _e345.y, _e345.z, _e178.w);
    }
    let _e352 = phi_355_;
    let _e354 = phi_356_;
    let _e356 = phi_357_;
    let _e379 = select(0u, 1u, (_e138.w < 0f));
    if (_e379 != 0u) {
        phi_383_ = -(_e50.w);
    } else {
        phi_383_ = _e50.w;
    }
    let _e385 = phi_383_;
    let _e386 = _e78.xyz;
    let _e387 = bitcast<vec3<u32>>(_e386);
    let _e388 = (_e387 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e394 = _e98.xyz;
    let _e395 = bitcast<vec3<u32>>(_e394);
    let _e396 = (_e395 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e402 = _e118.xyz;
    let _e403 = bitcast<vec3<u32>>(_e402);
    let _e404 = (_e403 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e411 = max(max(max(_e388.x, _e388.y), _e388.z), max(max(max(_e396.x, _e396.y), _e396.z), max(max(_e404.x, _e404.y), _e404.z)));
    switch bitcast<i32>(0u) {
        default: {
            phi_416_ = true;
            if (_e411 != 0u) {
                phi_416_ = (_e411 >= 2139095040u);
            }
            let _e416 = phi_416_;
            phi_425_ = true;
            if !(_e416) {
                phi_424_ = false;
                if (_e411 >= 841731191u) {
                    phi_424_ = (_e411 <= 1287568416u);
                }
                let _e421 = phi_424_;
                phi_425_ = _e421;
            }
            let _e423 = phi_425_;
            if _e423 {
                phi_432_ = 0i;
                break;
            }
            phi_432_ = clamp((127i - bitcast<i32>((_e411 >> bitcast<u32>(23u)))), -126i, 126i);
            break;
        }
    }
    let _e430 = phi_432_;
    phi_504_ = _e78;
    phi_505_ = _e98;
    phi_506_ = _e118;
    if (_e430 != 0i) {
        switch bitcast<i32>(0u) {
            default: {
                let _e437 = bitcast<f32>((bitcast<u32>((_e430 + 127i)) << bitcast<u32>(23u)));
                let _e438 = (_e386 * _e437);
                let _e439 = (_e430 <= 22i);
                local_4 = _e437;
                local_5 = _e439;
                local_6 = _e437;
                local_7 = _e439;
                if _e439 {
                    phi_460_ = _e438;
                    break;
                }
                phi_460_ = select(_e438, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e387 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e430 - 22i)) << bitcast<u32>(23u))))) | (_e387 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e387 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e456 = phi_460_;
        switch bitcast<i32>(0u) {
            default: {
                let _e464 = local_4;
                let _e465 = (_e394 * _e464);
                let _e467 = local_5;
                if _e467 {
                    phi_481_ = _e465;
                    break;
                }
                phi_481_ = select(_e465, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e395 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e430 - 22i)) << bitcast<u32>(23u))))) | (_e395 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e395 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e484 = phi_481_;
        switch bitcast<i32>(0u) {
            default: {
                let _e492 = local_6;
                let _e493 = (_e402 * _e492);
                let _e495 = local_7;
                if _e495 {
                    phi_502_ = _e493;
                    break;
                }
                phi_502_ = select(_e493, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e403 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e430 - 22i)) << bitcast<u32>(23u))))) | (_e403 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e403 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e512 = phi_502_;
        phi_504_ = vec4<f32>(_e456.x, _e456.y, _e456.z, _e78.w);
        phi_505_ = vec4<f32>(_e484.x, _e484.y, _e484.z, _e98.w);
        phi_506_ = vec4<f32>(_e512.x, _e512.y, _e512.z, _e118.w);
    }
    let _e519 = phi_504_;
    let _e521 = phi_505_;
    let _e523 = phi_506_;
    let _e543 = (((vec3<f32>(_e519.x, _e521.x, _e523.x) * _e50.x) + (vec3<f32>(_e519.y, _e521.y, _e523.y) * _e50.y)) + (vec3<f32>(_e519.z, _e521.z, _e523.z) * _e50.z));
    out_u002e_var_u002e_TEXCOORD1_ = (((vec3<f32>(_e352.x, _e354.x, _e356.x) * _e49.x) + (vec3<f32>(_e352.y, _e354.y, _e356.y) * _e49.y)) + (vec3<f32>(_e352.z, _e354.z, _e356.z) * _e49.z));
    out_u002e_var_u002e_TEXCOORD2_ = vec4<f32>(_e543.x, _e543.y, _e543.z, _e385);
    out_u002e_var_u002e_TEXCOORD3_ = _e51;
    out_u002e_var_u002e_TEXCOORD4_ = _e379;
    out_u002e_var_u002e_TEXCOORD5_ = vec2<f32>(0f, 0f);
    out_u002e_var_u002e_TEXCOORD6_ = 0u;
    global_1 = ((((_e205[0] * _e203.x) + (_e205[1] * _e203.y)) + (_e205[2] * _e203.z)) + _e205[3]);
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
    let _e18 = global_1.y;
    global_1.y = -(_e18);
    let _e20 = out_u002e_var_u002e_TEXCOORD1_;
    let _e21 = out_u002e_var_u002e_TEXCOORD2_;
    let _e22 = out_u002e_var_u002e_TEXCOORD3_;
    let _e23 = out_u002e_var_u002e_TEXCOORD4_;
    let _e24 = out_u002e_var_u002e_TEXCOORD5_;
    let _e25 = out_u002e_var_u002e_TEXCOORD6_;
    let _e26 = global_1;
    return VertexOutput(_e20, _e21, _e22, _e23, _e24, _e25, _e26);
}

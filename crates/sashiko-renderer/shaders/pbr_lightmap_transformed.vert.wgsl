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
var<private> in_u002e_var_u002e_TEXCOORD4_1: vec3<f32>;
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
    var phi_270_: bool;
    var phi_278_: bool;
    var phi_279_: bool;
    var phi_286_: i32;
    var phi_314_: vec3<f32>;
    var local: f32;
    var local_1: bool;
    var phi_335_: vec3<f32>;
    var local_2: f32;
    var local_3: bool;
    var phi_356_: vec3<f32>;
    var phi_358_: vec4<f32>;
    var phi_359_: vec4<f32>;
    var phi_360_: vec4<f32>;
    var phi_386_: f32;
    var phi_419_: bool;
    var phi_427_: bool;
    var phi_428_: bool;
    var phi_435_: i32;
    var phi_463_: vec3<f32>;
    var local_4: f32;
    var local_5: bool;
    var phi_484_: vec3<f32>;
    var local_6: f32;
    var local_7: bool;
    var phi_505_: vec3<f32>;
    var phi_507_: vec4<f32>;
    var phi_508_: vec4<f32>;
    var phi_509_: vec4<f32>;

    let _e50 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e51 = in_u002e_var_u002e_TEXCOORD1_1;
    let _e52 = in_u002e_var_u002e_TEXCOORD2_1;
    let _e53 = in_u002e_var_u002e_TEXCOORD3_1;
    let _e54 = in_u002e_var_u002e_TEXCOORD4_1;
    let _e55 = global;
    let _e57 = parameters.first_transform;
    let _e58 = min(_e57, 1023u);
    let _e62 = ((_e58 + min(_e55, (1023u - _e58))) * 96u);
    let _e64 = (_e62 >> bitcast<u32>(2u));
    let _e67 = transform_table.member[_e64];
    let _e71 = transform_table.member[(_e64 + 1u)];
    let _e75 = transform_table.member[(_e64 + 2u)];
    let _e79 = transform_table.member[(_e64 + 3u)];
    let _e81 = bitcast<vec4<f32>>(vec4<u32>(_e67, _e71, _e75, _e79));
    let _e84 = ((_e62 + 16u) >> bitcast<u32>(2u));
    let _e87 = transform_table.member[_e84];
    let _e91 = transform_table.member[(_e84 + 1u)];
    let _e95 = transform_table.member[(_e84 + 2u)];
    let _e99 = transform_table.member[(_e84 + 3u)];
    let _e101 = bitcast<vec4<f32>>(vec4<u32>(_e87, _e91, _e95, _e99));
    let _e104 = ((_e62 + 32u) >> bitcast<u32>(2u));
    let _e107 = transform_table.member[_e104];
    let _e111 = transform_table.member[(_e104 + 1u)];
    let _e115 = transform_table.member[(_e104 + 2u)];
    let _e119 = transform_table.member[(_e104 + 3u)];
    let _e121 = bitcast<vec4<f32>>(vec4<u32>(_e107, _e111, _e115, _e119));
    let _e124 = ((_e62 + 48u) >> bitcast<u32>(2u));
    let _e127 = transform_table.member[_e124];
    let _e131 = transform_table.member[(_e124 + 1u)];
    let _e135 = transform_table.member[(_e124 + 2u)];
    let _e139 = transform_table.member[(_e124 + 3u)];
    let _e141 = bitcast<vec4<f32>>(vec4<u32>(_e127, _e131, _e135, _e139));
    let _e144 = ((_e62 + 64u) >> bitcast<u32>(2u));
    let _e147 = transform_table.member[_e144];
    let _e151 = transform_table.member[(_e144 + 1u)];
    let _e155 = transform_table.member[(_e144 + 2u)];
    let _e159 = transform_table.member[(_e144 + 3u)];
    let _e161 = bitcast<vec4<f32>>(vec4<u32>(_e147, _e151, _e155, _e159));
    let _e164 = ((_e62 + 80u) >> bitcast<u32>(2u));
    let _e167 = transform_table.member[_e164];
    let _e171 = transform_table.member[(_e164 + 1u)];
    let _e175 = transform_table.member[(_e164 + 2u)];
    let _e179 = transform_table.member[(_e164 + 3u)];
    let _e181 = bitcast<vec4<f32>>(vec4<u32>(_e167, _e171, _e175, _e179));
    let _e206 = ((((vec3<f32>(_e81.x, _e101.x, _e121.x) * _e50.x) + (vec3<f32>(_e81.y, _e101.y, _e121.y) * _e50.y)) + (vec3<f32>(_e81.z, _e101.z, _e121.z) * _e50.z)) + vec3<f32>(_e81.w, _e101.w, _e121.w));
    let _e208 = camera.view_projection;
    let _e222 = _e141.xyz;
    let _e223 = bitcast<vec3<u32>>(_e222);
    let _e224 = (_e223 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e230 = _e161.xyz;
    let _e231 = bitcast<vec3<u32>>(_e230);
    let _e232 = (_e231 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e238 = _e181.xyz;
    let _e239 = bitcast<vec3<u32>>(_e238);
    let _e240 = (_e239 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e247 = max(max(max(_e224.x, _e224.y), _e224.z), max(max(max(_e232.x, _e232.y), _e232.z), max(max(_e240.x, _e240.y), _e240.z)));
    switch bitcast<i32>(0u) {
        default: {
            phi_270_ = true;
            if (_e247 != 0u) {
                phi_270_ = (_e247 >= 2139095040u);
            }
            let _e252 = phi_270_;
            phi_279_ = true;
            if !(_e252) {
                phi_278_ = false;
                if (_e247 >= 841731191u) {
                    phi_278_ = (_e247 <= 1287568416u);
                }
                let _e257 = phi_278_;
                phi_279_ = _e257;
            }
            let _e259 = phi_279_;
            if _e259 {
                phi_286_ = 0i;
                break;
            }
            phi_286_ = clamp((127i - bitcast<i32>((_e247 >> bitcast<u32>(23u)))), -126i, 126i);
            break;
        }
    }
    let _e266 = phi_286_;
    phi_358_ = _e141;
    phi_359_ = _e161;
    phi_360_ = _e181;
    if (_e266 != 0i) {
        switch bitcast<i32>(0u) {
            default: {
                let _e273 = bitcast<f32>((bitcast<u32>((_e266 + 127i)) << bitcast<u32>(23u)));
                let _e274 = (_e222 * _e273);
                let _e275 = (_e266 <= 22i);
                local = _e273;
                local_1 = _e275;
                local_2 = _e273;
                local_3 = _e275;
                if _e275 {
                    phi_314_ = _e274;
                    break;
                }
                phi_314_ = select(_e274, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e223 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e266 - 22i)) << bitcast<u32>(23u))))) | (_e223 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e223 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e292 = phi_314_;
        switch bitcast<i32>(0u) {
            default: {
                let _e300 = local;
                let _e301 = (_e230 * _e300);
                let _e303 = local_1;
                if _e303 {
                    phi_335_ = _e301;
                    break;
                }
                phi_335_ = select(_e301, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e231 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e266 - 22i)) << bitcast<u32>(23u))))) | (_e231 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e231 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e320 = phi_335_;
        switch bitcast<i32>(0u) {
            default: {
                let _e328 = local_2;
                let _e329 = (_e238 * _e328);
                let _e331 = local_3;
                if _e331 {
                    phi_356_ = _e329;
                    break;
                }
                phi_356_ = select(_e329, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e239 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e266 - 22i)) << bitcast<u32>(23u))))) | (_e239 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e239 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e348 = phi_356_;
        phi_358_ = vec4<f32>(_e292.x, _e292.y, _e292.z, _e141.w);
        phi_359_ = vec4<f32>(_e320.x, _e320.y, _e320.z, _e161.w);
        phi_360_ = vec4<f32>(_e348.x, _e348.y, _e348.z, _e181.w);
    }
    let _e355 = phi_358_;
    let _e357 = phi_359_;
    let _e359 = phi_360_;
    let _e382 = select(0u, 1u, (_e141.w < 0f));
    if (_e382 != 0u) {
        phi_386_ = -(_e52.w);
    } else {
        phi_386_ = _e52.w;
    }
    let _e388 = phi_386_;
    let _e389 = _e81.xyz;
    let _e390 = bitcast<vec3<u32>>(_e389);
    let _e391 = (_e390 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e397 = _e101.xyz;
    let _e398 = bitcast<vec3<u32>>(_e397);
    let _e399 = (_e398 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e405 = _e121.xyz;
    let _e406 = bitcast<vec3<u32>>(_e405);
    let _e407 = (_e406 & vec3<u32>(2147483647u, 2147483647u, 2147483647u));
    let _e414 = max(max(max(_e391.x, _e391.y), _e391.z), max(max(max(_e399.x, _e399.y), _e399.z), max(max(_e407.x, _e407.y), _e407.z)));
    switch bitcast<i32>(0u) {
        default: {
            phi_419_ = true;
            if (_e414 != 0u) {
                phi_419_ = (_e414 >= 2139095040u);
            }
            let _e419 = phi_419_;
            phi_428_ = true;
            if !(_e419) {
                phi_427_ = false;
                if (_e414 >= 841731191u) {
                    phi_427_ = (_e414 <= 1287568416u);
                }
                let _e424 = phi_427_;
                phi_428_ = _e424;
            }
            let _e426 = phi_428_;
            if _e426 {
                phi_435_ = 0i;
                break;
            }
            phi_435_ = clamp((127i - bitcast<i32>((_e414 >> bitcast<u32>(23u)))), -126i, 126i);
            break;
        }
    }
    let _e433 = phi_435_;
    phi_507_ = _e81;
    phi_508_ = _e101;
    phi_509_ = _e121;
    if (_e433 != 0i) {
        switch bitcast<i32>(0u) {
            default: {
                let _e440 = bitcast<f32>((bitcast<u32>((_e433 + 127i)) << bitcast<u32>(23u)));
                let _e441 = (_e389 * _e440);
                let _e442 = (_e433 <= 22i);
                local_4 = _e440;
                local_5 = _e442;
                local_6 = _e440;
                local_7 = _e442;
                if _e442 {
                    phi_463_ = _e441;
                    break;
                }
                phi_463_ = select(_e441, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e390 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e433 - 22i)) << bitcast<u32>(23u))))) | (_e390 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e390 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e459 = phi_463_;
        switch bitcast<i32>(0u) {
            default: {
                let _e467 = local_4;
                let _e468 = (_e397 * _e467);
                let _e470 = local_5;
                if _e470 {
                    phi_484_ = _e468;
                    break;
                }
                phi_484_ = select(_e468, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e398 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e433 - 22i)) << bitcast<u32>(23u))))) | (_e398 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e398 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e487 = phi_484_;
        switch bitcast<i32>(0u) {
            default: {
                let _e495 = local_6;
                let _e496 = (_e405 * _e495);
                let _e498 = local_7;
                if _e498 {
                    phi_505_ = _e496;
                    break;
                }
                phi_505_ = select(_e496, bitcast<vec3<f32>>((bitcast<vec3<u32>>((vec3<f32>((_e406 & vec3<u32>(8388607u, 8388607u, 8388607u))) * bitcast<f32>((bitcast<u32>((_e433 - 22i)) << bitcast<u32>(23u))))) | (_e406 & vec3<u32>(2147483648u, 2147483648u, 2147483648u)))), ((_e406 & vec3<u32>(2139095040u, 2139095040u, 2139095040u)) == vec3<u32>(0u, 0u, 0u)));
                break;
            }
        }
        let _e515 = phi_505_;
        phi_507_ = vec4<f32>(_e459.x, _e459.y, _e459.z, _e81.w);
        phi_508_ = vec4<f32>(_e487.x, _e487.y, _e487.z, _e101.w);
        phi_509_ = vec4<f32>(_e515.x, _e515.y, _e515.z, _e121.w);
    }
    let _e522 = phi_507_;
    let _e524 = phi_508_;
    let _e526 = phi_509_;
    let _e546 = (((vec3<f32>(_e522.x, _e524.x, _e526.x) * _e52.x) + (vec3<f32>(_e522.y, _e524.y, _e526.y) * _e52.y)) + (vec3<f32>(_e522.z, _e524.z, _e526.z) * _e52.z));
    let _e553 = parameters.slot;
    out_u002e_var_u002e_TEXCOORD0_ = _e206;
    out_u002e_var_u002e_TEXCOORD1_ = (((vec3<f32>(_e355.x, _e357.x, _e359.x) * _e51.x) + (vec3<f32>(_e355.y, _e357.y, _e359.y) * _e51.y)) + (vec3<f32>(_e355.z, _e357.z, _e359.z) * _e51.z));
    out_u002e_var_u002e_TEXCOORD2_ = vec4<f32>(_e546.x, _e546.y, _e546.z, _e388);
    out_u002e_var_u002e_TEXCOORD3_ = _e53;
    out_u002e_var_u002e_TEXCOORD4_ = _e382;
    out_u002e_var_u002e_TEXCOORD5_ = _e54.xy;
    out_u002e_var_u002e_TEXCOORD6_ = (_e553 + u32(_e54.z));
    global_1 = ((((_e208[0] * _e206.x) + (_e208[1] * _e206.y)) + (_e208[2] * _e206.z)) + _e208[3]);
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
    let _e21 = global_1.y;
    global_1.y = -(_e21);
    let _e23 = out_u002e_var_u002e_TEXCOORD0_;
    let _e24 = out_u002e_var_u002e_TEXCOORD1_;
    let _e25 = out_u002e_var_u002e_TEXCOORD2_;
    let _e26 = out_u002e_var_u002e_TEXCOORD3_;
    let _e27 = out_u002e_var_u002e_TEXCOORD4_;
    let _e28 = out_u002e_var_u002e_TEXCOORD5_;
    let _e29 = out_u002e_var_u002e_TEXCOORD6_;
    let _e30 = global_1;
    return VertexOutput(_e23, _e24, _e25, _e26, _e27, _e28, _e29, _e30);
}

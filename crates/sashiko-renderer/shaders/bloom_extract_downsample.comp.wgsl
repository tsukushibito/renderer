@group(0) @binding(0)
var source_image: texture_2d<f32>;
@group(1) @binding(0)
var destination_image: texture_storage_2d<rgba16float,write>;
var<private> global: vec3<u32>;

fn main_1() {
    let _e20 = global;
    switch bitcast<i32>(0u) {
        default: {
            let _e22 = _e20.xy;
            let _e23 = bitcast<vec2<i32>>(_e22);
            let _e24 = textureDimensions(destination_image);
            if any((_e23 >= bitcast<vec2<i32>>(_e24))) {
                break;
            }
            let _e28 = textureDimensions(source_image, 0u);
            let _e30 = (bitcast<vec2<i32>>(_e28) - vec2<i32>(1i, 1i));
            let _e32 = min((_e23 * vec2<i32>(2i, 2i)), _e30);
            let _e34 = min((_e32 + vec2<i32>(1i, 1i)), _e30);
            let _e39 = textureLoad(source_image, vec3<i32>(_e32.x, _e32.y, 0i).xy, 0i);
            let _e41 = max(_e39.xyz, vec3<f32>(0f, 0f, 0f));
            let _e46 = max(max(_e41.x, _e41.y), _e41.z);
            let _e51 = (_e41 * (max((_e46 - 1f), 0f) / max(_e46, 1f)));
            let _e63 = textureLoad(source_image, vec3<i32>(_e34.x, _e32.y, 0i).xy, 0i);
            let _e65 = max(_e63.xyz, vec3<f32>(0f, 0f, 0f));
            let _e70 = max(max(_e65.x, _e65.y), _e65.z);
            let _e75 = (_e65 * (max((_e70 - 1f), 0f) / max(_e70, 1f)));
            let _e88 = textureLoad(source_image, vec3<i32>(_e32.x, _e34.y, 0i).xy, 0i);
            let _e90 = max(_e88.xyz, vec3<f32>(0f, 0f, 0f));
            let _e95 = max(max(_e90.x, _e90.y), _e90.z);
            let _e100 = (_e90 * (max((_e95 - 1f), 0f) / max(_e95, 1f)));
            let _e112 = textureLoad(source_image, vec3<i32>(_e34.x, _e34.y, 0i).xy, 0i);
            let _e114 = max(_e112.xyz, vec3<f32>(0f, 0f, 0f));
            let _e119 = max(max(_e114.x, _e114.y), _e114.z);
            let _e124 = (_e114 * (max((_e119 - 1f), 0f) / max(_e119, 1f)));
            let _e134 = ((((select(bitcast<vec3<f32>>((bitcast<vec3<u32>>(_e51) & vec3<u32>(4294959104u, 4294959104u, 4294959104u))), (trunc((_e51 * 16777216f)) * vec3<f32>(0.000000059604645f, 0.000000059604645f, 0.000000059604645f)), (_e51 < vec3<f32>(0.000061035156f, 0.000061035156f, 0.000061035156f))) + select(bitcast<vec3<f32>>((bitcast<vec3<u32>>(_e75) & vec3<u32>(4294959104u, 4294959104u, 4294959104u))), (trunc((_e75 * 16777216f)) * vec3<f32>(0.000000059604645f, 0.000000059604645f, 0.000000059604645f)), (_e75 < vec3<f32>(0.000061035156f, 0.000061035156f, 0.000061035156f)))) + select(bitcast<vec3<f32>>((bitcast<vec3<u32>>(_e100) & vec3<u32>(4294959104u, 4294959104u, 4294959104u))), (trunc((_e100 * 16777216f)) * vec3<f32>(0.000000059604645f, 0.000000059604645f, 0.000000059604645f)), (_e100 < vec3<f32>(0.000061035156f, 0.000061035156f, 0.000061035156f)))) + select(bitcast<vec3<f32>>((bitcast<vec3<u32>>(_e124) & vec3<u32>(4294959104u, 4294959104u, 4294959104u))), (trunc((_e124 * 16777216f)) * vec3<f32>(0.000000059604645f, 0.000000059604645f, 0.000000059604645f)), (_e124 < vec3<f32>(0.000061035156f, 0.000061035156f, 0.000061035156f)))) * 0.25f);
            textureStore(destination_image, _e22, vec4<f32>(_e134.x, _e134.y, _e134.z, 0f));
            break;
        }
    }
    return;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) param: vec3<u32>) {
    global = param;
    main_1();
}

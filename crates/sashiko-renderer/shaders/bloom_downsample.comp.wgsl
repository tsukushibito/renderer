@group(0) @binding(0)
var source_image: texture_2d<f32>;
@group(1) @binding(0)
var destination_image: texture_storage_2d<rgba16float,write>;
var<private> global: vec3<u32>;

fn main_1() {
    let _e11 = global;
    switch bitcast<i32>(0u) {
        default: {
            let _e13 = _e11.xy;
            let _e14 = bitcast<vec2<i32>>(_e13);
            let _e15 = textureDimensions(destination_image);
            if any((_e14 >= bitcast<vec2<i32>>(_e15))) {
                break;
            }
            let _e19 = textureDimensions(source_image, 0u);
            let _e21 = (bitcast<vec2<i32>>(_e19) - vec2<i32>(1i, 1i));
            let _e23 = min((_e14 * vec2<i32>(2i, 2i)), _e21);
            let _e25 = min((_e23 + vec2<i32>(1i, 1i)), _e21);
            let _e30 = textureLoad(source_image, vec3<i32>(_e23.x, _e23.y, 0i).xy, 0i);
            let _e35 = textureLoad(source_image, vec3<i32>(_e25.x, _e23.y, 0i).xy, 0i);
            let _e41 = textureLoad(source_image, vec3<i32>(_e23.x, _e25.y, 0i).xy, 0i);
            let _e46 = textureLoad(source_image, vec3<i32>(_e25.x, _e25.y, 0i).xy, 0i);
            let _e49 = ((((_e30.xyz + _e35.xyz) + _e41.xyz) + _e46.xyz) * 0.25f);
            textureStore(destination_image, _e13, vec4<f32>(_e49.x, _e49.y, _e49.z, 0f));
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

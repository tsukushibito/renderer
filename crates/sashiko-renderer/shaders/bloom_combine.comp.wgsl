@group(0) @binding(0)
var source_image: texture_2d<f32>;
@group(1) @binding(0)
var destination_image: texture_storage_2d<rgba16float,write>;
@group(0) @binding(1)
var blur_image: texture_2d<f32>;
var<private> global: vec3<u32>;

fn main_1() {
    let _e15 = global;
    switch bitcast<i32>(0u) {
        default: {
            let _e17 = _e15.xy;
            let _e18 = bitcast<vec2<i32>>(_e17);
            let _e19 = textureDimensions(destination_image);
            let _e20 = bitcast<vec2<i32>>(_e19);
            if any((_e18 >= _e20)) {
                break;
            }
            let _e23 = textureDimensions(blur_image, 0u);
            let _e24 = bitcast<vec2<i32>>(_e23);
            let _e32 = textureDimensions(source_image, 0u);
            let _e35 = min(_e18, (bitcast<vec2<i32>>(_e32) - vec2<i32>(1i, 1i)));
            let _e40 = textureLoad(source_image, vec3<i32>(_e35.x, _e35.y, 0i).xy, 0i);
            let _e42 = (_e24 - vec2<i32>(1i, 1i));
            let _e44 = clamp((((vec2<f32>(_e18) + vec2<f32>(0.5f, 0.5f)) * (vec2<f32>(_e24) / vec2<f32>(_e20))) - vec2<f32>(0.5f, 0.5f)), vec2<f32>(0f, 0f), vec2<f32>(_e42));
            let _e45 = vec2<i32>(_e44);
            let _e47 = min((_e45 + vec2<i32>(1i, 1i)), _e42);
            let _e49 = (_e44 - vec2<f32>(_e45));
            let _e54 = textureLoad(blur_image, vec3<i32>(_e45.x, _e45.y, 0i).xy, 0i);
            let _e59 = textureLoad(blur_image, vec3<i32>(_e47.x, _e45.y, 0i).xy, 0i);
            let _e62 = vec3(_e49.x);
            let _e67 = textureLoad(blur_image, vec3<i32>(_e45.x, _e47.y, 0i).xy, 0i);
            let _e71 = textureLoad(blur_image, vec3<i32>(_e47.x, _e47.y, 0i).xy, 0i);
            let _e79 = min((_e40.xyz + (mix(mix(_e54.xyz, _e59.xyz, _e62), mix(_e67.xyz, _e71.xyz, _e62), vec3(_e49.y)) * 0.125f)), vec3<f32>(65504f, 65504f, 65504f));
            textureStore(destination_image, _e17, vec4<f32>(_e79.x, _e79.y, _e79.z, _e40.w));
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

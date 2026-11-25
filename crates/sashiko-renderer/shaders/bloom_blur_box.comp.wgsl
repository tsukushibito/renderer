@group(0) @binding(0)
var source_image: texture_2d<f32>;
@group(1) @binding(0)
var destination_image: texture_storage_2d<rgba16float,write>;
var<private> global: vec3<u32>;

fn main_1() {
    var phi_51_: vec3<f32>;
    var phi_54_: i32;
    var phi_52_: vec3<f32>;
    var phi_62_: i32;
    var local: vec3<f32>;
    var local_1: vec3<f32>;

    let _e12 = global;
    switch bitcast<i32>(0u) {
        default: {
            let _e14 = _e12.xy;
            let _e15 = bitcast<vec2<i32>>(_e14);
            let _e16 = textureDimensions(destination_image);
            if any((_e15 >= bitcast<vec2<i32>>(_e16))) {
                break;
            }
            let _e20 = textureDimensions(source_image, 0u);
            phi_51_ = vec3<f32>(0f, 0f, 0f);
            phi_54_ = -1i;
            loop {
                let _e24 = phi_51_;
                let _e26 = phi_54_;
                local = _e24;
                if (_e26 <= 1i) {
                    phi_52_ = _e24;
                    phi_62_ = -1i;
                    loop {
                        let _e29 = phi_52_;
                        let _e31 = phi_62_;
                        local_1 = _e29;
                        if (_e31 <= 1i) {
                            continue;
                        } else {
                            break;
                        }
                        continuing {
                            let _e35 = clamp((_e15 + vec2<i32>(_e31, _e26)), vec2<i32>(0i, 0i), (bitcast<vec2<i32>>(_e20) - vec2<i32>(1i, 1i)));
                            let _e40 = textureLoad(source_image, vec3<i32>(_e35.x, _e35.y, 0i).xy, 0i);
                            phi_52_ = (_e29 + _e40.xyz);
                            phi_62_ = (_e31 + 1i);
                        }
                    }
                    continue;
                } else {
                    break;
                }
                continuing {
                    let _e54 = local_1;
                    phi_51_ = _e54;
                    phi_54_ = (_e26 + 1i);
                }
            }
            let _e46 = local;
            let _e47 = (_e46 * 0.11111111f);
            textureStore(destination_image, _e14, vec4<f32>(_e47.x, _e47.y, _e47.z, 0f));
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

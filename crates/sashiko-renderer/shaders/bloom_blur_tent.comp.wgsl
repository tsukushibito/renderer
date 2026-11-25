@group(0) @binding(0)
var source_image: texture_2d<f32>;
@group(1) @binding(0)
var destination_image: texture_storage_2d<rgba16float,write>;
var<private> global: vec3<u32>;

fn main_1() {
    var phi_52_: vec3<f32>;
    var phi_55_: i32;
    var phi_53_: vec3<f32>;
    var phi_63_: i32;
    var local: vec3<f32>;
    var local_1: vec3<f32>;

    let _e13 = global;
    switch bitcast<i32>(0u) {
        default: {
            let _e15 = _e13.xy;
            let _e16 = bitcast<vec2<i32>>(_e15);
            let _e17 = textureDimensions(destination_image);
            if any((_e16 >= bitcast<vec2<i32>>(_e17))) {
                break;
            }
            let _e21 = textureDimensions(source_image, 0u);
            phi_52_ = vec3<f32>(0f, 0f, 0f);
            phi_55_ = -1i;
            loop {
                let _e25 = phi_52_;
                let _e27 = phi_55_;
                local = _e25;
                if (_e27 <= 1i) {
                    phi_53_ = _e25;
                    phi_63_ = -1i;
                    loop {
                        let _e30 = phi_53_;
                        let _e32 = phi_63_;
                        local_1 = _e30;
                        if (_e32 <= 1i) {
                            continue;
                        } else {
                            break;
                        }
                        continuing {
                            let _e36 = clamp((_e16 + vec2<i32>(_e32, _e27)), vec2<i32>(0i, 0i), (bitcast<vec2<i32>>(_e21) - vec2<i32>(1i, 1i)));
                            let _e47 = textureLoad(source_image, vec3<i32>(_e36.x, _e36.y, 0i).xy, 0i);
                            phi_53_ = (_e30 + (_e47.xyz * f32(((2i - abs(_e32)) * (2i - abs(_e27))))));
                            phi_63_ = (_e32 + 1i);
                        }
                    }
                    continue;
                } else {
                    break;
                }
                continuing {
                    let _e62 = local_1;
                    phi_52_ = _e62;
                    phi_55_ = (_e27 + 1i);
                }
            }
            let _e54 = local;
            let _e55 = (_e54 * 0.0625f);
            textureStore(destination_image, _e15, vec4<f32>(_e55.x, _e55.y, _e55.z, 0f));
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

struct type_u002e_ConstantBuffer_u002e_MaterialData {
    base_factor: vec4<f32>,
    emissive_normal_scale: vec4<f32>,
    metallic_roughness_occlusion_cutoff: vec4<f32>,
    flags: vec4<u32>,
}

@group(0) @binding(1)
var<uniform> material: type_u002e_ConstantBuffer_u002e_MaterialData;
@group(1) @binding(0)
var base_image: texture_2d<f32>;
@group(1) @binding(4)
var material_sampler: sampler;
var<private> in_u002e_var_u002e_TEXCOORD0_1: vec2<f32>;
var<private> global: bool;

fn main_1() {
    var phi_55_: bool;
    var phi_56_: bool;

    let _e16 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e17 = global;
    let _e18 = dpdx(_e16);
    let _e19 = dpdy(_e16);
    let _e22 = material.flags[0i];
    phi_56_ = true;
    if (_e22 != 2u) {
        phi_55_ = false;
        if !(_e17) {
            let _e27 = material.flags[1i];
            phi_55_ = (_e27 == 0u);
        }
        let _e30 = phi_55_;
        phi_56_ = _e30;
    }
    let _e32 = phi_56_;
    if _e32 {
        discard;
    }
    if (_e22 == 1u) {
        let _e36 = material.base_factor[3i];
        let _e37 = textureSampleGrad(base_image, material_sampler, _e16, _e18, _e19);
        let _e43 = material.metallic_roughness_occlusion_cutoff[3i];
        if (clamp((_e36 * _e37.w), 0f, 1f) < _e43) {
            discard;
        }
    }
    return;
}

@fragment
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec2<f32>, @builtin(front_facing) param: bool) {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    global = param;
    main_1();
}

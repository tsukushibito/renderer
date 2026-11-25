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
var<private> in_u002e_var_u002e_TEXCOORD1_1: u32;
var<private> global: bool;

fn main_1() {
    var phi_59_: bool;
    var phi_60_: bool;

    let _e17 = in_u002e_var_u002e_TEXCOORD0_1;
    let _e18 = in_u002e_var_u002e_TEXCOORD1_1;
    let _e19 = global;
    let _e21 = dpdx(_e17);
    let _e22 = dpdy(_e17);
    let _e25 = material.flags[0i];
    phi_60_ = true;
    if (_e25 != 2u) {
        phi_59_ = false;
        if (_e19 == (_e18 != 0u)) {
            let _e30 = material.flags[1i];
            phi_59_ = (_e30 == 0u);
        }
        let _e33 = phi_59_;
        phi_60_ = _e33;
    }
    let _e35 = phi_60_;
    if _e35 {
        discard;
    }
    if (_e25 == 1u) {
        let _e39 = material.base_factor[3i];
        let _e40 = textureSampleGrad(base_image, material_sampler, _e17, _e21, _e22);
        let _e46 = material.metallic_roughness_occlusion_cutoff[3i];
        if (clamp((_e39 * _e40.w), 0f, 1f) < _e46) {
            discard;
        }
    }
    return;
}

@fragment
fn main(@location(0) in_u002e_var_u002e_TEXCOORD0_: vec2<f32>, @location(1) @interpolate(flat) in_u002e_var_u002e_TEXCOORD1_: u32, @builtin(front_facing) param: bool) {
    in_u002e_var_u002e_TEXCOORD0_1 = in_u002e_var_u002e_TEXCOORD0_;
    in_u002e_var_u002e_TEXCOORD1_1 = in_u002e_var_u002e_TEXCOORD1_;
    global = param;
    main_1();
}

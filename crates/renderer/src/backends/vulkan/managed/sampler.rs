//! Sampler configurations shared by Graph pipelines.

use crate::program::SamplerDescriptor;
use ash::vk;

/// Reviewed feature-free sampler state, shared by Compute and Raster.
pub(in crate::backends::vulkan) fn sampler_info(
    desc: SamplerDescriptor,
) -> vk::SamplerCreateInfo<'static> {
    let linear = desc.is_linear();
    let repeat_u = matches!(
        desc,
        SamplerDescriptor::LinearRepeat | SamplerDescriptor::LinearRepeatUClampV
    );
    let repeat_vw = desc == SamplerDescriptor::LinearRepeat;
    vk::SamplerCreateInfo::default()
        .min_filter(if linear {
            vk::Filter::LINEAR
        } else {
            vk::Filter::NEAREST
        })
        .mag_filter(if linear {
            vk::Filter::LINEAR
        } else {
            vk::Filter::NEAREST
        })
        .mipmap_mode(if linear {
            vk::SamplerMipmapMode::LINEAR
        } else {
            vk::SamplerMipmapMode::NEAREST
        })
        .address_mode_u(if repeat_u {
            vk::SamplerAddressMode::REPEAT
        } else {
            vk::SamplerAddressMode::CLAMP_TO_EDGE
        })
        .address_mode_v(if repeat_vw {
            vk::SamplerAddressMode::REPEAT
        } else {
            vk::SamplerAddressMode::CLAMP_TO_EDGE
        })
        .address_mode_w(if repeat_vw {
            vk::SamplerAddressMode::REPEAT
        } else {
            vk::SamplerAddressMode::CLAMP_TO_EDGE
        })
        .min_lod(0.0)
        .max_lod(if linear { vk::LOD_CLAMP_NONE } else { 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linear_policies_preserve_the_selected_view_mips_and_axis_addressing() {
        for (policy, u, v) in [
            (
                SamplerDescriptor::LinearRepeat,
                vk::SamplerAddressMode::REPEAT,
                vk::SamplerAddressMode::REPEAT,
            ),
            (
                SamplerDescriptor::LinearClamp,
                vk::SamplerAddressMode::CLAMP_TO_EDGE,
                vk::SamplerAddressMode::CLAMP_TO_EDGE,
            ),
            (
                SamplerDescriptor::LinearRepeatUClampV,
                vk::SamplerAddressMode::REPEAT,
                vk::SamplerAddressMode::CLAMP_TO_EDGE,
            ),
        ] {
            let info = sampler_info(policy);
            assert_eq!(info.min_filter, vk::Filter::LINEAR);
            assert_eq!(info.mag_filter, vk::Filter::LINEAR);
            assert_eq!(info.mipmap_mode, vk::SamplerMipmapMode::LINEAR);
            assert_eq!(info.address_mode_u, u);
            assert_eq!(info.address_mode_v, v);
            assert_eq!(info.address_mode_w, v);
            assert_eq!(info.max_lod, vk::LOD_CLAMP_NONE);
            assert_eq!(info.anisotropy_enable, vk::FALSE);
            assert_eq!(info.compare_enable, vk::FALSE);
        }
        let info = sampler_info(SamplerDescriptor::NearestClamp);
        assert_eq!(info.min_filter, vk::Filter::NEAREST);
        assert_eq!(info.mag_filter, vk::Filter::NEAREST);
        assert_eq!(info.max_lod, 0.0);
    }
}

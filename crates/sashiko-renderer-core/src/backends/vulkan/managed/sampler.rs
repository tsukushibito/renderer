//! Sampler configurations shared by Graph pipelines.

use crate::program::SamplerDescriptor;
use ash::vk;

/// Reviewed feature-free sampler state, shared by Compute and Raster.
pub(in crate::backends::vulkan) fn sampler_info(
    desc: SamplerDescriptor,
) -> vk::SamplerCreateInfo<'static> {
    use crate::program::{AddressMode, FilterMode, MipFilter};
    let filter = |value| match value {
        FilterMode::Nearest => vk::Filter::NEAREST,
        FilterMode::Linear => vk::Filter::LINEAR,
    };
    let address = |value| match value {
        AddressMode::Repeat => vk::SamplerAddressMode::REPEAT,
        AddressMode::ClampToEdge => vk::SamplerAddressMode::CLAMP_TO_EDGE,
        AddressMode::MirroredRepeat => vk::SamplerAddressMode::MIRRORED_REPEAT,
    };
    // VkSamplerCreateInfo's OpenGL filter mapping preserves minFilter by
    // allowing positive lambda, but rounds to level zero in nearest mip mode.
    let max_lod = match desc.mip_filter {
        MipFilter::None if desc.min_filter == desc.mag_filter => 0.0,
        MipFilter::None => 0.25,
        MipFilter::Nearest | MipFilter::Linear => vk::LOD_CLAMP_NONE,
    };
    vk::SamplerCreateInfo::default()
        .compare_enable(desc.compare_depth)
        .compare_op(vk::CompareOp::LESS_OR_EQUAL)
        .min_filter(filter(desc.min_filter))
        .mag_filter(filter(desc.mag_filter))
        .mipmap_mode(match desc.mip_filter {
            MipFilter::None | MipFilter::Nearest => vk::SamplerMipmapMode::NEAREST,
            MipFilter::Linear => vk::SamplerMipmapMode::LINEAR,
        })
        .address_mode_u(address(desc.wrap_u))
        .address_mode_v(address(desc.wrap_v))
        .address_mode_w(address(desc.wrap_v))
        .min_lod(0.0)
        .max_lod(max_lod)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gltf_non_mipmap_filters_keep_base_level_and_independent_minification() {
        use crate::program::{AddressMode, FilterMode, MipFilter};
        let desc = SamplerDescriptor {
            compare_depth: false,
            wrap_u: AddressMode::MirroredRepeat,
            wrap_v: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Nearest,
            min_filter: FilterMode::Linear,
            mip_filter: MipFilter::None,
        };
        let info = sampler_info(desc);
        assert_eq!(info.address_mode_u, vk::SamplerAddressMode::MIRRORED_REPEAT);
        assert_eq!(info.address_mode_v, vk::SamplerAddressMode::CLAMP_TO_EDGE);
        assert_eq!(info.mag_filter, vk::Filter::NEAREST);
        assert_eq!(info.min_filter, vk::Filter::LINEAR);
        assert_eq!(info.mipmap_mode, vk::SamplerMipmapMode::NEAREST);
        assert_eq!(info.max_lod, 0.25);
    }
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

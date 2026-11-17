//! Immutable sampler ownership using the Device-wide allocation quota.

use super::{MemoryOwner, RuntimeError};
use crate::program::SamplerDescriptor;
use ash::vk;
use std::sync::Arc;

/// Reviewed feature-free sampler state, shared by managed and Graph ownership.
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

#[derive(Clone)]
pub struct VulkanSampler(pub(super) Arc<Sampler>);
impl VulkanSampler {
    pub fn desc(&self) -> SamplerDescriptor {
        self.0.desc
    }
    pub(in crate::backends::vulkan) fn create(
        owner: Arc<MemoryOwner>,
        desc: SamplerDescriptor,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let info = sampler_info(desc);
        // SAFETY: Reviewed fixed configuration uses no optional features. The
        // Device helper reserves its quota before creation, with error rollback.
        let handle = unsafe { owner.device.create_sampler(&info) }?;
        Ok(Self(Arc::new(Sampler {
            owner,
            handle,
            desc,
        })))
    }
}
pub(super) struct Sampler {
    pub(super) owner: Arc<MemoryOwner>,
    pub(super) handle: vk::Sampler,
    desc: SamplerDescriptor,
}
impl Drop for Sampler {
    fn drop(&mut self) {
        // SAFETY: Bindings/work retain the sampler through native use. The
        // helper destroys the native object before returning quota capacity.
        unsafe { self.owner.device.destroy_sampler(self.handle) };
    }
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

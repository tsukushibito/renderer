//! Immutable sampler ownership using the Device-wide allocation quota.

use super::{MemoryOwner, RuntimeError};
use crate::program::SamplerDescriptor;
use ash::vk;
use std::sync::Arc;

/// Reviewed feature-free sampler state, shared by managed and Graph ownership.
pub(in crate::backends::vulkan) fn sampler_info(
    desc: SamplerDescriptor,
) -> vk::SamplerCreateInfo<'static> {
    match desc {
        SamplerDescriptor::NearestClamp => vk::SamplerCreateInfo::default()
            .min_filter(vk::Filter::NEAREST)
            .mag_filter(vk::Filter::NEAREST)
            .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .min_lod(0.0)
            .max_lod(0.0),
        SamplerDescriptor::LinearRepeat => vk::SamplerCreateInfo::default()
            .min_filter(vk::Filter::LINEAR)
            .mag_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::REPEAT)
            .address_mode_v(vk::SamplerAddressMode::REPEAT)
            .address_mode_w(vk::SamplerAddressMode::REPEAT)
            .min_lod(0.0)
            .max_lod(vk::LOD_CLAMP_NONE),
    }
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

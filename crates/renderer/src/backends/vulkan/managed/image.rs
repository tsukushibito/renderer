//! Single-subresource sampled image and separately owned View creation.

use super::{MemoryOwner, RuntimeError, native};
use crate::program::{
    ImageDescriptor, ImageFormat, ImageViewDescriptor, ImageViewUsage, validate_image_descriptor,
    validate_image_view_descriptor,
};
use ash::vk;
use std::sync::Arc;
use vk_mem::Alloc;

fn native_format(format: ImageFormat) -> vk::Format {
    match format {
        ImageFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        ImageFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        ImageFormat::D32Float => vk::Format::D32_SFLOAT,
    }
}

fn native_aspect(format: ImageFormat) -> vk::ImageAspectFlags {
    match format {
        ImageFormat::D32Float => vk::ImageAspectFlags::DEPTH,
        ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float => vk::ImageAspectFlags::COLOR,
    }
}

/// An uninitialized sampled image with one mip, layer and sample.
#[derive(Clone)]
pub struct VulkanImage(pub(super) Arc<Image>);
impl VulkanImage {
    pub fn desc(&self) -> ImageDescriptor {
        self.0.desc
    }
    pub(in crate::backends::vulkan) fn create(
        owner: Arc<MemoryOwner>,
        desc: ImageDescriptor,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        validate_image_descriptor(desc, owner.device.limits().max_image_dimension2_d)
            .map_err(|error| RuntimeError::invalid(error.to_string()))?;
        let format = native_format(desc.format);
        let usage = vk::ImageUsageFlags::SAMPLED
            | match desc.format {
                ImageFormat::D32Float => vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
                ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float => {
                    vk::ImageUsageFlags::TRANSFER_DST
                }
            };
        let query = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage);
        let mut support = vk::ImageFormatProperties2::default();
        // SAFETY: The retained Device/Instance own this adapter. Query exactly
        // the format/type/tiling/usage/flags used for native creation below.
        unsafe {
            owner
                .device
                .instance_raw()
                .get_physical_device_image_format_properties2(
                    owner.device.physical_device(),
                    &query,
                    &mut support,
                )
        }
        .map_err(|r| {
            native(
                &owner,
                "vkGetPhysicalDeviceImageFormatProperties2(sampled)",
                r,
            )
        })?;
        let limits = support.image_format_properties;
        if desc.width > limits.max_extent.width
            || desc.height > limits.max_extent.height
            || limits.max_extent.depth < 1
            || limits.max_mip_levels < 1
            || limits.max_array_layers < 1
            || !limits.sample_counts.contains(vk::SampleCountFlags::TYPE_1)
        {
            return Err(RuntimeError::unsupported(
                "sampled image exceeds exact format/usage capabilities",
            ));
        }
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let allocation_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        // SAFETY: Checked exact request. VMA binds an unaliased image allocation
        // and is retained by its immediate owner, including partial failures.
        let (handle, allocation) = unsafe { owner.raw.create_image(&info, &allocation_info) }
            .map_err(|r| native(&owner, "vmaCreateImage(sampled)", r))?;
        Ok(Self(Arc::new(Image {
            owner,
            handle,
            allocation,
            desc,
            #[cfg(test)]
            state: ImageState::default(),
        })))
    }
}
pub(super) struct Image {
    pub(super) owner: Arc<MemoryOwner>,
    pub(super) handle: vk::Image,
    allocation: vk_mem::Allocation,
    pub(super) desc: ImageDescriptor,
    #[cfg(test)]
    pub(super) state: ImageState,
}
#[cfg(test)]
#[derive(Default)]
pub(super) struct ImageState {
    pub(super) busy: std::sync::atomic::AtomicBool,
    pub(super) initialized: std::sync::atomic::AtomicBool,
}
impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: Views/bindings/work retain the image through all recorded and
        // GPU uses. No allocation, layout mutation or mapping escapes publicly.
        unsafe {
            self.owner
                .raw
                .destroy_image(self.handle, &mut self.allocation)
        };
    }
}

/// An immutable complete 2D View, retaining its image and fixed interpretation.
#[derive(Clone)]
pub struct VulkanImageView(pub(super) Arc<ImageView>);
impl VulkanImageView {
    pub fn desc(&self) -> ImageViewDescriptor {
        self.0.desc
    }
    pub(in crate::backends::vulkan) fn create(
        owner: &Arc<MemoryOwner>,
        image: &VulkanImage,
        desc: ImageViewDescriptor,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if !Arc::ptr_eq(owner, &image.0.owner) {
            return Err(RuntimeError::wrong_owner(
                "image View belongs to another Device",
            ));
        }
        validate_image_view_descriptor(image.desc().format, desc)
            .map_err(|e| RuntimeError::invalid(e.to_string()))?;
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(native_aspect(image.desc().format))
            .base_mip_level(desc.base_mip_level)
            .level_count(desc.mip_level_count)
            .base_array_layer(desc.base_array_layer)
            .layer_count(desc.array_layer_count);
        let mut usage = vk::ImageViewUsageCreateInfo::default().usage(match desc.usage {
            ImageViewUsage::Sampled => vk::ImageUsageFlags::SAMPLED,
            ImageViewUsage::DepthAttachment => vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
        });
        let info = vk::ImageViewCreateInfo::default()
            .image(image.0.handle)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(native_format(image.desc().format))
            .subresource_range(range)
            .push_next(&mut usage);
        // SAFETY: Same-Device immutable image retains its creation format and
        // usage. This View uses that exact format and a checked full subresource.
        let handle = unsafe { owner.device.raw().create_image_view(&info, None) }
            .map_err(|r| native(owner, "vkCreateImageView(managed)", r))?;
        Ok(Self(Arc::new(ImageView {
            image: image.clone(),
            handle,
            desc,
        })))
    }
}
pub(super) struct ImageView {
    pub(super) image: VulkanImage,
    pub(super) handle: vk::ImageView,
    desc: ImageViewDescriptor,
}
impl Drop for ImageView {
    fn drop(&mut self) {
        // SAFETY: Binding/work owners retain the View until retirement/cancel.
        // Its image and Device are released only after destroying this View.
        unsafe {
            self.image
                .0
                .owner
                .device
                .raw()
                .destroy_image_view(self.handle, None)
        };
    }
}

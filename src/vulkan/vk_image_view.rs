use std::sync::Arc;

use ash::vk;

use super::super::Result;
use super::{vk_device::VkDevice, vk_image::VkImage, vk_image::VkSwapchainImage};

pub struct VkImageViewCreateInfo {
    pub flags: vk::ImageViewCreateFlags,
    pub view_type: vk::ImageViewType,
    pub format: vk::Format,
    pub components: vk::ComponentMapping,
    pub subresource_range: vk::ImageSubresourceRange,
}

#[derive(Clone)]
pub enum VkImageResource {
    Image(Arc<VkImage>),
    SwapchainImage(Arc<VkSwapchainImage>),
}

impl VkImageResource {
    fn image(&self) -> vk::Image {
        match self {
            VkImageResource::Image(i) => i.native_handle,
            VkImageResource::SwapchainImage(i) => i.native_handle,
        }
    }
}

pub struct VkImageView {
    device: Arc<VkDevice>,
    pub(crate) native_handle: vk::ImageView,
    pub(crate) image_resource: VkImageResource,
    pub(crate) create_info: vk::ImageViewCreateInfo,
}

impl VkImageView {
    pub fn new(
        device: &Arc<VkDevice>,
        image_resource: &VkImageResource,
        create_info: VkImageViewCreateInfo,
    ) -> Result<Self> {
        let info = vk::ImageViewCreateInfo::builder()
            .image(image_resource.image())
            .flags(create_info.flags)
            .view_type(create_info.view_type)
            .format(create_info.format)
            .components(create_info.components)
            .subresource_range(create_info.subresource_range);
        let image_view = unsafe { device.native_handle.create_image_view(&info, None) }?;

        Ok(Self {
            device: device.clone(),
            image_resource: image_resource.clone(),
            create_info: *info,
            native_handle: image_view,
        })
    }
}

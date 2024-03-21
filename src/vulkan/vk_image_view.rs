use std::sync::Arc;

use ash::vk;

use super::super::Result;
use super::{vk_device::VkDevice, vk_image::VkImage};

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
    SwapchainImage(Arc<VkImage>),
}

impl VkImageResource {
    fn image(&self) -> vk::Image {
        match self {
            VkImageResource::Image(i) => i.image,
            VkImageResource::SwapchainImage(i) => i.image,
        }
    }
}

pub struct VkImageView {
    device: Arc<VkDevice>,
    pub(crate) image_resource: VkImageResource,
    pub(crate) create_info: vk::ImageViewCreateInfo,
    pub(crate) image_view: vk::ImageView,
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
        let image_view = unsafe { device.device.create_image_view(&info, None) }?;

        Ok(Self {
            device: device.clone(),
            image_resource: image_resource.clone(),
            create_info: *info,
            image_view,
        })
    }
}

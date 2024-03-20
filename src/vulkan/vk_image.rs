use std::sync::Arc;

use ash::vk;

use super::VkDevice;

pub struct VkImage {
    device: Arc<VkDevice>,
    is_swapchain_image: bool,
    pub(crate) image: vk::Image,

    pub(crate) extent: vk::Extent2D,
    pub(crate) format: vk::Format,
    pub(crate) usage: vk::ImageUsageFlags,
}

impl VkImage {
    pub fn new(
        device: &Arc<VkDevice>,
        extent: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) {
        todo!()
    }

    pub(crate) fn new_swapchain_image(
        device: &Arc<VkDevice>,
        image: vk::Image,
        extent: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Self {
        Self {
            device: device.clone(),
            is_swapchain_image: true,
            image,
            extent,
            format,
            usage,
        }
    }
}

impl Drop for VkImage {
    fn drop(&mut self) {
        if !self.is_swapchain_image {
            unsafe { self.device.device.destroy_image(self.image, None) };
        }
    }
}

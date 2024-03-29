use std::sync::Arc;

use ash::vk;

use super::super::Result;
use super::vk_device::VkDevice;

pub struct VkImage {
    device: Arc<VkDevice>,
    pub(crate) native_handle: vk::Image,

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
}

impl Drop for VkImage {
    fn drop(&mut self) {
        unsafe {
            self.device
                .native_handle
                .destroy_image(self.native_handle, None)
        };
    }
}

pub struct VkSwapchainImage {
    pub(crate) native_handle: vk::Image,

    pub(crate) extent: vk::Extent2D,
    pub(crate) format: vk::Format,
    pub(crate) usage: vk::ImageUsageFlags,
}

impl VkSwapchainImage {
    pub(crate) fn new(
        swapchain_image: vk::Image,
        extent: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Self {
        Self {
            native_handle: swapchain_image,
            extent,
            format,
            usage,
        }
    }
}

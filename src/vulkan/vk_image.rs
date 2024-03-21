use std::sync::Arc;

use ash::vk;

use super::vk_device::VkDevice;

pub struct VkImage {
    device: Arc<VkDevice>,
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
}

impl Drop for VkImage {
    fn drop(&mut self) {
        unsafe { self.device.device.destroy_image(self.image, None) };
    }
}

use std::sync::Arc;

use ash::vk;

use super::VkDevice;

pub(crate) struct VkImageView {
    device: Arc<VkDevice>,
    image_view: vk::ImageView,
    create_info: vk::ImageViewCreateInfo,
}

impl VkImageView {
    pub fn new(device: &Arc<VkDevice>) -> Self {
        todo!()
    }
}

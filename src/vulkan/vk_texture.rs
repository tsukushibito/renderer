use std::sync::Arc;

use ash::vk;

use super::{vk_image::VkImage, VkDevice};

pub struct VkTexture {
    device: Arc<VkDevice>,
    pub(crate) image: Arc<VkImage>,
    pub(crate) image_view: vk::ImageView,
}

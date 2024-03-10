use std::sync::Arc;

use ash::vk;

use super::{vk_image::VkImage, VkDevice};

pub struct Texture {
    device: Arc<VkDevice>,
    pub(crate) image: Arc<VkImage>,
    pub(crate) image_view: vk::ImageView,
}

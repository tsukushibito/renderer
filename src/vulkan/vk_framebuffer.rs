use std::sync::Arc;

use ash::vk;

use super::{vk_device::VkDevice, vk_image_view::VkImageView};

pub struct VkFramebuffer {
    device: Arc<VkDevice>,
    pub(crate) native_handle: vk::Framebuffer,
    pub(crate) color_image_views: Vec<Arc<VkImageView>>,
    pub(crate) depth_image_view: Arc<VkImageView>,
    pub(crate) render_pass: vk::RenderPass,
}

impl VkFramebuffer {
    fn new() -> Self {
        todo!()
    }
}

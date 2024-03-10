use std::sync::Arc;

use ash::vk;

use super::VkDevice;

pub struct VkFramebuffer {
    device: Arc<VkDevice>,
    pub(crate) framebuffer: vk::Framebuffer,
    pub(crate) render_pass: vk::RenderPass,
}

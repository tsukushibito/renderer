use std::sync::Arc;

use ash::vk;

use super::{VkDevice, VkTexture};

pub struct VkFramebuffer {
    device: Arc<VkDevice>,
    pub(crate) color_textures: Vec<Arc<VkTexture>>,
    pub(crate) depth_texture: Arc<VkTexture>,
    pub(crate) framebuffer: vk::Framebuffer,
    pub(crate) render_pass: vk::RenderPass,
}

impl VkFramebuffer {
    fn new() -> Self {}
}

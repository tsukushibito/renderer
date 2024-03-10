pub struct VkSwapchainImage {
    pub(crate) image: vk::Image,
    pub(crate) extent: vk::Extent2D,
    pub(crate) format: vk::Format,
    pub(crate) usage: vk::ImageUsageFlags,
}

pub(crate) struct VkFrameResource {
    pub(crate) image: Arc<VkSwapchainImage>,
}

pub struct VkSwapchain {}

use std::{cell::Cell, sync::Arc};

use ash::vk;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use super::VkDevice;

pub struct VkSwapchainImage {
    pub(crate) image: vk::Image,
    pub(crate) extent: vk::Extent2D,
    pub(crate) format: vk::Format,
    pub(crate) usage: vk::ImageUsageFlags,
}

pub struct VkSwapchainTexture {
    pub(crate) image: Arc<VkSwapchainImage>,
    pub(crate) image_view: vk::ImageView,
    pub(crate) image_view_info: vk::ImageViewCreateInfo,
}

pub(crate) struct VkFrameResource {
    pub texture: Arc<VkSwapchainTexture>,
    pub command_pool: vk::CommandPool,
    pub command_buffer: vk::CommandBuffer,
    pub image_available_semaphore: vk::Semaphore,
    pub render_finished_semaphore: vk::Semaphore,
    pub in_flight_fence: vk::Fence,
}

pub struct VkSwapchain {
    device: Arc<VkDevice>,
    surface_loader: ash::extensions::khr::Surface,
    swapchain_loader: ash::extensions::khr::Swapchain,
    display_handle: RawDisplayHandle,
    window_handle: RawWindowHandle,
    surface: vk::SurfaceKHR,
    pub(crate) swapchain: vk::SwapchainKHR,
    pub(crate) image_format: vk::Format,
    pub(crate) image_extent: vk::Extent2D,
    image_color_space: vk::ColorSpaceKHR,
    present_mode: vk::PresentModeKHR,
    frame_resources: Vec<VkFrameResource>,
    current_frame: Cell<usize>,
}

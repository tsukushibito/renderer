use std::{cell::Cell, sync::Arc};

use ash::vk;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use super::super::Result;
use super::{vk_device::VkDevice, vk_framebuffer::VkFramebuffer, vk_texture::VkTexture};

pub(crate) struct VkFrameResource {
    pub texture: Arc<VkTexture>,
    pub framebuffer: Arc<VkFramebuffer>,
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
    swapchain: vk::SwapchainKHR,
    surface: vk::SurfaceKHR,
    image_color_space: vk::ColorSpaceKHR,
    present_mode: vk::PresentModeKHR,
    frame_resources: Vec<Arc<VkFrameResource>>,
    current_frame: Cell<usize>,
}

impl VkSwapchain {
    fn acquire_next_frame_resource(&self) -> Result<(Arc<VkFrameResource>, bool)> {
        let current_frame = self.current_frame.get();
        let next_frame = (current_frame + 1) % self.frame_resources.len();
        let semaphre = &self.frame_resources[next_frame].image_available_semaphore;
        let (index, is_suboptimal) = unsafe {
            self.swapchain_loader.acquire_next_image(
                self.swapchain,
                std::u64::MAX,
                *semaphre,
                vk::Fence::null(),
            )?
        };
        let next_frame = index;
        Ok((self.frame_resources[index as usize].clone(), is_suboptimal))
    }
}

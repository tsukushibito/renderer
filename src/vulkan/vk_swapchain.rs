use std::{cell::Cell, sync::Arc};

use ash::vk::{self, SurfaceFormat2KHR};

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use super::super::Result;
use super::vk_device::VkDevice;
use super::vk_framebuffer::VkFramebuffer;
use super::vk_image::VkSwapchainImage;
use super::vk_image_view::VkImageView;
use super::vk_physical_device::VkPhysicalDevice;
use super::VkRenderBackend;

pub(crate) struct VkFrameResource {
    pub image_view: Arc<VkImageView>,
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
    native_handle: vk::SwapchainKHR,
    surface: vk::SurfaceKHR,
    image_color_space: vk::ColorSpaceKHR,
    present_mode: vk::PresentModeKHR,
    frame_resources: Vec<Arc<VkFrameResource>>,
    current_frame: Cell<usize>,
}

impl VkSwapchain {
    pub fn new(
        backend: &Arc<VkRenderBackend>,
        device: &Arc<VkDevice>,
        display_handle: &RawDisplayHandle,
        window_handle: &RawWindowHandle,
        extent: &vk::Extent2D,
    ) -> Result<Self> {
        let surface_loader = ash::extensions::khr::Surface::new(&backend.entry, &backend.instance);
        let swapchain_loader =
            ash::extensions::khr::Swapchain::new(&backend.instance, &device.native_handle);
        todo!()
    }

    pub(crate) fn acquire_next_frame_resource(&self) -> Result<(Arc<VkFrameResource>, bool)> {
        let current_frame = self.current_frame.get();
        let next_frame = (current_frame + 1) % self.frame_resources.len();
        let semaphre = &self.frame_resources[next_frame].image_available_semaphore;
        let (index, is_suboptimal) = unsafe {
            self.swapchain_loader.acquire_next_image(
                self.native_handle,
                std::u64::MAX,
                *semaphre,
                vk::Fence::null(),
            )?
        };
        let next_frame = index;
        Ok((self.frame_resources[index as usize].clone(), is_suboptimal))
    }
}

struct SwapchainResources {
    swapchain: vk::SwapchainKHR,
    surface: vk::SurfaceKHR,
    image_color_space: vk::ColorSpaceKHR,
    present_mode: vk::PresentModeKHR,
    frame_resources: Vec<Arc<VkFrameResource>>,
    current_frame: Cell<usize>,
}

fn create_swapchain_resources(
    backend: &VkRenderBackend,
    device: &VkDevice,
    swapchain_loader: &ash::extensions::khr::Swapchain,
    display_handle: &RawDisplayHandle,
    window_handle: &RawWindowHandle,
    window_extent: &vk::Extent2D,
) -> Result<SwapchainResources> {
    let entry = &backend.entry;
    let instance = &backend.instance;
    let surface_loader = &backend.surface_loader;
    let surface = unsafe {
        ash_window::create_surface(entry, instance, *display_handle, *window_handle, None)?
    };

    let physical_device = device.physical_device;
    let (swapchain, surface_format, extent, image_usage) = create_swapchain(
        surface_loader,
        swapchain_loader,
        &physical_device,
        surface,
        window_extent,
    )?;

    let frame_resources = create_frame_resources(
        swapchain_loader,
        &swapchain,
        &surface_format,
        device,
        extent,
        image_usage,
    );

    todo!()
}

fn create_swapchain(
    surface_loader: &ash::extensions::khr::Surface,
    swapchain_loader: &ash::extensions::khr::Swapchain,
    physical_device: &VkPhysicalDevice,
    surface: vk::SurfaceKHR,
    window_extent: &vk::Extent2D,
) -> Result<(
    vk::SwapchainKHR,
    vk::SurfaceFormatKHR,
    vk::Extent2D,
    vk::ImageUsageFlags,
)> {
    let surface_format =
        choose_swapchain_format(&surface_loader, &physical_device.native_handle, &surface)?;
    let present_mode =
        choose_swapchain_present_mode(&surface_loader, &physical_device.native_handle, &surface)?;
    let surface_capabilities = unsafe {
        surface_loader
            .get_physical_device_surface_capabilities(physical_device.native_handle, surface)?
    };
    let image_count = std::cmp::min(
        surface_capabilities.min_image_count + 1,
        surface_capabilities.max_image_count,
    );
    let surface_resolution = if surface_capabilities.current_extent.width == std::u32::MAX {
        *window_extent
    } else {
        surface_capabilities.current_extent
    };

    let image_usage = vk::ImageUsageFlags::COLOR_ATTACHMENT;
    let mut swapchain_create_info = vk::SwapchainCreateInfoKHR::builder()
        .surface(surface)
        .min_image_count(image_count)
        .image_format(surface_format.format)
        .image_color_space(surface_format.color_space)
        .image_extent(surface_resolution)
        .image_array_layers(1)
        .image_usage(image_usage)
        .pre_transform(surface_capabilities.current_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(present_mode)
        .clipped(true);

    let queue_family_indices = [
        physical_device.graphics_family,
        physical_device.present_family,
    ];

    if queue_family_indices[0] != queue_family_indices[1] {
        swapchain_create_info = swapchain_create_info
            .image_sharing_mode(vk::SharingMode::CONCURRENT)
            .queue_family_indices(&queue_family_indices);
    } else {
        swapchain_create_info =
            swapchain_create_info.image_sharing_mode(vk::SharingMode::EXCLUSIVE);
    }

    let swapchain_create_info = swapchain_create_info.build();

    let swapchain = unsafe { swapchain_loader.create_swapchain(&swapchain_create_info, None)? };

    Ok((swapchain, surface_format, surface_resolution, image_usage))
}

fn choose_swapchain_format(
    surface_loader: &ash::extensions::khr::Surface,
    physical_device: &vk::PhysicalDevice,
    surface: &vk::SurfaceKHR,
) -> Result<vk::SurfaceFormatKHR> {
    let formats =
        unsafe { surface_loader.get_physical_device_surface_formats(*physical_device, *surface)? };

    for &format in &formats {
        if format.format == vk::Format::B8G8R8A8_SRGB
            && format.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        {
            return Ok(format);
        }
    }

    Ok(formats[0])
}

fn choose_swapchain_present_mode(
    surface_loader: &ash::extensions::khr::Surface,
    physical_device: &vk::PhysicalDevice,
    surface: &vk::SurfaceKHR,
) -> Result<vk::PresentModeKHR> {
    let present_modes = unsafe {
        surface_loader.get_physical_device_surface_present_modes(*physical_device, *surface)?
    };

    for mode in present_modes {
        if mode == vk::PresentModeKHR::MAILBOX {
            return Ok(mode);
        }
    }

    Ok(vk::PresentModeKHR::FIFO)
}

fn create_frame_resources(
    swapchain_loader: &ash::extensions::khr::Swapchain,
    swapchain: &vk::SwapchainKHR,
    surface_format: &vk::SurfaceFormatKHR,
    device: &VkDevice,
    extent: vk::Extent2D,
    image_usage: vk::ImageUsageFlags,
) -> Result<Vec<Arc<VkFrameResource>>> {
    let images = unsafe { swapchain_loader.get_swapchain_images(*swapchain)? };
    let images = images
        .iter()
        .map(|&image| VkSwapchainImage::new(image, extent, surface_format.format, image_usage))
        .collect::<Vec<VkSwapchainImage>>();
    let image_views = images
        .iter()
        .map(|&image| {
            let view_type = vk::ImageViewType::TYPE_2D;
            let format = surface_format.format;
            let components = vk::ComponentMapping {
                r: vk::ComponentSwizzle::R,
                g: vk::ComponentSwizzle::G,
                b: vk::ComponentSwizzle::B,
                a: vk::ComponentSwizzle::A,
            };
            let subresource_range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };

            let image_view_create_info = vk::ImageViewCreateInfo::builder()
                .image(image.native_handle)
                .view_type(view_type)
                .format(format)
                .components(components)
                .subresource_range(subresource_range)
                .build();

            let image_view = unsafe {
                device
                    .native_handle
                    .create_image_view(&image_view_create_info, None)
            };
            image_view.map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        })
        .collect::<Result<Vec<vk::ImageView>>>()?;

    let image_count = images.len();
    let command_pools = (0..image_count)
        .map(|_| {
            create_command_pool(
                &device.native_handle,
                device.physical_device.graphics_family,
            )
        })
        .collect::<Result<Vec<vk::CommandPool>>>()?;

    let tmp = command_pools
        .iter()
        .map(|&command_pool| allocate_command_buffers(&device.native_handle, command_pool, 1))
        .collect::<Result<Vec<Vec<vk::CommandBuffer>>>>()?;
    let command_buffers = tmp
        .into_iter()
        .flat_map(|cb| cb)
        .collect::<Vec<vk::CommandBuffer>>();

    let image_available_semaphores = (0..image_count)
        .map(|_| {
            let semaphore_create_info = vk::SemaphoreCreateInfo::builder();
            let result = unsafe {
                device
                    .native_handle
                    .create_semaphore(&semaphore_create_info, None)
            };
            result.map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        })
        .collect::<Result<Vec<vk::Semaphore>>>()?;

    let render_finished_semaphores = (0..image_count)
        .map(|_| {
            let semaphore_create_info = vk::SemaphoreCreateInfo::builder();
            let result = unsafe {
                device
                    .native_handle
                    .create_semaphore(&semaphore_create_info, None)
            };
            result.map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        })
        .collect::<Result<Vec<vk::Semaphore>>>()?;

    let in_flight_fences = (0..image_count)
        .map(|_| {
            let fence_create_info =
                vk::FenceCreateInfo::builder().flags(vk::FenceCreateFlags::SIGNALED);
            let result = unsafe { device.native_handle.create_fence(&fence_create_info, None) };
            result.map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        })
        .collect::<Result<Vec<vk::Fence>>>()?;

    let frame_resources = (0..image_count)
        .map(|i| VkFrameResource {
            image: images[i],
            image_view: image_views[i],
            command_pool: command_pools[i],
            command_buffer: command_buffers[i],
            image_available_semaphore: image_available_semaphores[i],
            render_finished_semaphore: render_finished_semaphores[i],
            in_flight_fence: in_flight_fences[i],
        })
        .collect::<Vec<VkFrameResource>>();
}

fn create_command_pool(device: &ash::Device, queue_family_index: u32) -> Result<vk::CommandPool> {
    let command_pool_create_info =
        vk::CommandPoolCreateInfo::builder().queue_family_index(queue_family_index);
    let command_pool = unsafe { device.create_command_pool(&command_pool_create_info, None)? };
    Ok(command_pool)
}

fn allocate_command_buffers(
    device: &ash::Device,
    command_pool: vk::CommandPool,
    count: u32,
) -> Result<Vec<vk::CommandBuffer>> {
    let command_buffer_allocate_info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(count);

    let command_buffers =
        unsafe { device.allocate_command_buffers(&command_buffer_allocate_info)? };

    Ok(command_buffers)
}

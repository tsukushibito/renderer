//! Surface selection and Swapchain generation ownership.

use super::*;

fn choose_format(formats: &[vk::SurfaceFormatKHR]) -> Result<vk::SurfaceFormatKHR, RuntimeError> {
    [vk::Format::B8G8R8A8_SRGB, vk::Format::R8G8B8A8_SRGB]
        .into_iter()
        .find_map(|format| {
            formats
                .iter()
                .find(|entry| {
                    entry.format == format && entry.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
                })
                .copied()
        })
        .ok_or_else(|| {
            RuntimeError::unsupported(
                "presentation requires BGRA8_SRGB or RGBA8_SRGB with SRGB_NONLINEAR",
            )
        })
}
fn choose_extent(caps: &vk::SurfaceCapabilitiesKHR, desired: (u32, u32)) -> Option<vk::Extent2D> {
    if desired.0 == 0
        || desired.1 == 0
        || caps.max_image_extent.width == 0
        || caps.max_image_extent.height == 0
    {
        return None;
    }
    let value = if caps.current_extent.width != u32::MAX {
        caps.current_extent
    } else {
        vk::Extent2D {
            width: desired
                .0
                .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
            height: desired
                .1
                .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
        }
    };
    (value.width != 0 && value.height != 0).then_some(value)
}
fn image_count(caps: &vk::SurfaceCapabilitiesKHR, requested: u32) -> u32 {
    let value = requested.max(caps.min_image_count);
    if caps.max_image_count == 0 {
        value
    } else {
        value.min(caps.max_image_count)
    }
}

fn choose_present_mode(
    supported: &[vk::PresentModeKHR],
    vsync: bool,
) -> Result<vk::PresentModeKHR, RuntimeError> {
    let mode = if vsync {
        vk::PresentModeKHR::FIFO
    } else {
        vk::PresentModeKHR::IMMEDIATE
    };
    if supported.contains(&mode) {
        Ok(mode)
    } else {
        Err(RuntimeError::unsupported(format!(
            "Surface does not support requested VSync {} ({mode:?})",
            if vsync { "on" } else { "off" }
        )))
    }
}

pub(in crate::backends::vulkan) fn query_present_mode(
    owner: &MemoryOwner,
    surface: &WindowSurface,
    vsync: bool,
) -> Result<vk::PresentModeKHR, RuntimeError> {
    // SAFETY: This live Surface and physical Device were selected together.
    let supported = unsafe {
        surface.loader.get_physical_device_surface_present_modes(
            owner.device.physical_device(),
            surface.handle,
        )
    }
    .map_err(|r| native(owner, "vkGetPhysicalDeviceSurfacePresentModesKHR", r))?;
    choose_present_mode(&supported, vsync)
}
pub(in crate::backends::vulkan) struct Swapchain {
    owner: Arc<MemoryOwner>,
    _surface: Arc<WindowSurface>,
    pub(in crate::backends::vulkan) loader: ash::khr::swapchain::Device,
    pub(in crate::backends::vulkan) handle: vk::SwapchainKHR,
    pub(super) images: Vec<vk::Image>,
    pub(super) views: Vec<vk::ImageView>,
    pub(in crate::backends::vulkan) extent: vk::Extent2D,
    pub(in crate::backends::vulkan) format: vk::Format,
}
pub(in crate::backends::vulkan) struct SwapchainPlan {
    extent: vk::Extent2D,
    format: vk::SurfaceFormatKHR,
    count: u32,
    alpha: vk::CompositeAlphaFlagsKHR,
    transform: vk::SurfaceTransformFlagsKHR,
    present_mode: vk::PresentModeKHR,
}
impl Swapchain {
    pub(in crate::backends::vulkan) fn new(
        owner: Arc<MemoryOwner>,
        surface: Arc<WindowSurface>,
        plan: SwapchainPlan,
        old: vk::SwapchainKHR,
    ) -> Result<Arc<Self>, RuntimeError> {
        let SwapchainPlan {
            extent,
            format,
            count,
            alpha,
            transform,
            present_mode,
        } = plan;
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface.handle)
            .min_image_count(count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(transform)
            .composite_alpha(alpha)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(old);
        let loader =
            ash::khr::swapchain::Device::new(owner.device.instance_raw(), owner.device.raw());
        // SAFETY: Actual Surface capabilities were queried, one queue supports
        // graphics/compute/present, and old belongs to the same Surface. A caller
        // retains old even if creation fails, because this call retires it.
        let handle = unsafe { loader.create_swapchain(&info, None) }
            .map_err(|r| native(&owner, "vkCreateSwapchainKHR", r))?;
        let mut chain = Self {
            owner,
            _surface: surface,
            loader,
            handle,
            images: Vec::new(),
            views: Vec::new(),
            extent,
            format: format.format,
        };
        // SAFETY: The successful Swapchain is immediately owned by chain.
        chain.images = unsafe { chain.loader.get_swapchain_images(handle) }
            .map_err(|r| native(&chain.owner, "vkGetSwapchainImagesKHR", r))?;
        for &image in &chain.images {
            let info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format.format)
                .subresource_range(color_range());
            // SAFETY: Views reference this owned Swapchain's images; no VMA
            // Allocation or separate image ownership is fabricated for them.
            let view = unsafe { chain.owner.device.raw().create_image_view(&info, None) }
                .map_err(|r| native(&chain.owner, "vkCreateImageView(swapchain)", r))?;
            chain.views.push(view);
        }
        Ok(Arc::new(chain))
    }
}
impl Drop for Swapchain {
    fn drop(&mut self) {
        // SAFETY: Work and queued-present records retain this owner until both
        // GPU references and all corresponding present fences have completed.
        unsafe {
            for view in self.views.drain(..) {
                self.owner.device.raw().destroy_image_view(view, None);
            }
            self.loader.destroy_swapchain(self.handle, None);
        }
    }
}
impl SwapchainPlan {
    pub(in crate::backends::vulkan) fn query(
        owner: &MemoryOwner,
        surface: &WindowSurface,
        desired: (u32, u32),
        config: PresentationConfig,
    ) -> Result<Option<Self>, RuntimeError> {
        // SAFETY: Live Surface and physical Device were selected together.
        let caps = unsafe {
            surface.loader.get_physical_device_surface_capabilities(
                owner.device.physical_device(),
                surface.handle,
            )
        }
        .map_err(|r| native(owner, "vkGetPhysicalDeviceSurfaceCapabilitiesKHR", r))?;
        let Some(extent) = choose_extent(&caps, desired) else {
            return Ok(None);
        };
        // SAFETY: Same retained Surface and adapter.
        let formats = unsafe {
            surface
                .loader
                .get_physical_device_surface_formats(owner.device.physical_device(), surface.handle)
        }
        .map_err(|r| native(owner, "vkGetPhysicalDeviceSurfaceFormatsKHR", r))?;
        let format = choose_format(&formats)?;
        // Validate before retiring the old Swapchain. A mode change uses the
        // same bounded generation/lifetime path as a resize, without idle waits.
        let present_mode = query_present_mode(owner, surface, config.vsync)?;
        if !caps
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        {
            return Err(RuntimeError::unsupported(
                "Surface lacks COLOR_ATTACHMENT usage",
            ));
        }
        if !caps
            .supported_composite_alpha
            .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
        {
            return Err(RuntimeError::unsupported(
                "M3 presentation requires an OPAQUE Surface",
            ));
        }
        let alpha = vk::CompositeAlphaFlagsKHR::OPAQUE;
        let query = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(format.format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT);
        let mut support = vk::ImageFormatProperties2::default();
        // SAFETY: Check the actual implied Swapchain image creation parameters
        // before retiring the current Swapchain or creating its replacement.
        unsafe {
            owner
                .device
                .instance_raw()
                .get_physical_device_image_format_properties2(
                    owner.device.physical_device(),
                    &query,
                    &mut support,
                )
        }
        .map_err(|r| {
            native(
                owner,
                "vkGetPhysicalDeviceImageFormatProperties2(present)",
                r,
            )
        })?;
        if extent.width > support.image_format_properties.max_extent.width
            || extent.height > support.image_format_properties.max_extent.height
            || !support
                .image_format_properties
                .sample_counts
                .contains(vk::SampleCountFlags::TYPE_1)
        {
            return Err(RuntimeError::unsupported(
                "Swapchain image extent/sample count exceeds format support",
            ));
        }
        let plan = SwapchainPlan {
            extent,
            format,
            alpha,
            count: image_count(&caps, config.desired_images),
            transform: caps.current_transform,
            present_mode,
        };
        Ok(Some(plan))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimized_surface_never_creates_zero_extent_and_unbounded_count_is_not_zero() {
        let mut caps = vk::SurfaceCapabilitiesKHR {
            current_extent: vk::Extent2D {
                width: u32::MAX,
                height: u32::MAX,
            },
            min_image_extent: vk::Extent2D {
                width: 16,
                height: 16,
            },
            max_image_extent: vk::Extent2D {
                width: 1024,
                height: 768,
            },
            min_image_count: 2,
            ..Default::default()
        };
        assert_eq!(choose_extent(&caps, (0, 720)), None);
        assert_eq!(
            choose_extent(&caps, (1280, 1)),
            Some(vk::Extent2D {
                width: 1024,
                height: 16
            })
        );
        assert_eq!(image_count(&caps, 3), 3);
        caps.max_image_count = 2;
        assert_eq!(image_count(&caps, 3), 2);
        caps.current_extent = vk::Extent2D {
            width: 640,
            height: 480,
        };
        assert_eq!(choose_extent(&caps, (800, 600)), Some(caps.current_extent));
        caps.max_image_extent.width = 0;
        assert_eq!(choose_extent(&caps, (800, 600)), None);
    }

    #[test]
    fn linear_unorm_surface_is_not_silently_treated_as_srgb() {
        let unorm = vk::SurfaceFormatKHR {
            format: vk::Format::B8G8R8A8_UNORM,
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
        };
        assert!(choose_format(&[unorm]).is_err());
        let srgb = vk::SurfaceFormatKHR {
            format: vk::Format::R8G8B8A8_SRGB,
            ..unorm
        };
        assert_eq!(choose_format(&[unorm, srgb]).unwrap().format, srgb.format);
    }

    #[test]
    fn vsync_selects_only_the_requested_supported_mode() {
        let modes = [
            vk::PresentModeKHR::IMMEDIATE,
            vk::PresentModeKHR::FIFO,
            vk::PresentModeKHR::MAILBOX,
        ];
        assert!(PresentationConfig::default().vsync);
        assert_eq!(
            choose_present_mode(&modes, true).unwrap(),
            vk::PresentModeKHR::FIFO
        );
        assert_eq!(
            choose_present_mode(&modes, false).unwrap(),
            vk::PresentModeKHR::IMMEDIATE
        );
        let synchronized = [vk::PresentModeKHR::FIFO, vk::PresentModeKHR::MAILBOX];
        assert_eq!(
            choose_present_mode(&synchronized, false)
                .unwrap_err()
                .kind(),
            crate::backends::vulkan::error::RuntimeErrorKind::Unsupported
        );
        assert!(choose_present_mode(&[], true).is_err());
    }
}

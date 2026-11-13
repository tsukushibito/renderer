//! Window/Instance ownership for the deliberately limited Windows WSI path.

use std::sync::Arc;

use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

use super::diagnostics::DiagnosticsHandle;
use super::graph::RuntimeError;
use super::instance::{ValidationMode, VulkanInstance};

pub(super) struct WindowSurface {
    pub(super) instance: Arc<VulkanInstance>,
    pub(super) loader: ash::khr::surface::Instance,
    pub(super) handle: vk::SurfaceKHR,
    // The native Surface is destroyed before this last Window owner is dropped.
    _window: Arc<Window>,
}

impl WindowSurface {
    pub(super) fn new(
        window: Arc<Window>,
        diagnostics: DiagnosticsHandle,
        validation: ValidationMode,
    ) -> Result<Arc<Self>, RuntimeError> {
        let extensions = [
            ash::khr::surface::NAME,
            ash::khr::win32_surface::NAME,
            ash::khr::get_surface_capabilities2::NAME,
            ash::ext::surface_maintenance1::NAME,
        ];
        let instance = Arc::new(VulkanInstance::with_extensions(
            diagnostics,
            validation,
            &extensions,
        )?);
        let display = window.display_handle().map_err(|error| {
            RuntimeError::invalid(format!("Windows display handle unavailable: {error}"))
        })?;
        let handle = window.window_handle().map_err(|error| {
            RuntimeError::invalid(format!("Windows window handle unavailable: {error}"))
        })?;
        // SAFETY: These paired handles come from one live winit Window. The
        // returned owner keeps that Window, Instance and loader alive until the
        // Surface is destroyed. All platform extensions were checked/enabled.
        let surface = unsafe {
            ash_window::create_surface(
                instance.entry(),
                instance.raw(),
                display.as_raw(),
                handle.as_raw(),
                None,
            )
        }
        .map_err(|result| RuntimeError::native("vkCreateWin32SurfaceKHR", "pre-device", result))?;
        let loader = ash::khr::surface::Instance::new(instance.entry(), instance.raw());
        Ok(Arc::new(Self {
            instance,
            loader,
            handle: surface,
            _window: window,
        }))
    }

    pub(super) fn assess_device(
        &self,
        physical: vk::PhysicalDevice,
        queues: &mut [vk::QueueFamilyProperties],
    ) -> Result<bool, vk::Result> {
        for (index, queue) in queues.iter_mut().enumerate() {
            // SAFETY: Surface and adapter belong to the retained Instance.
            let supported = unsafe {
                self.loader
                    .get_physical_device_surface_support(physical, index as u32, self.handle)
            }?;
            if !supported {
                queue.queue_count = 0;
            }
        }
        // SAFETY: Queries use the same retained physical Device/Surface pair.
        let caps = unsafe {
            self.loader
                .get_physical_device_surface_capabilities(physical, self.handle)
        }?;
        // SAFETY: Same live pair; ash owns the enumeration storage.
        let formats = unsafe {
            self.loader
                .get_physical_device_surface_formats(physical, self.handle)
        }?;
        Ok(caps
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            && caps
                .supported_composite_alpha
                .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
            && formats.iter().any(|format| {
                format.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
                    && matches!(
                        format.format,
                        vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_SRGB
                    )
            }))
    }
}

impl Drop for WindowSurface {
    fn drop(&mut self) {
        // SAFETY: Every Swapchain owner retains this Surface. Its final Drop
        // therefore follows all Swapchain destruction, with Window/Instance live.
        unsafe { self.loader.destroy_surface(self.handle, None) };
    }
}

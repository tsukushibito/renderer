//! Window/Instance ownership for the deliberately limited Windows WSI path.

use std::sync::Arc;

use ash::vk;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::super::diagnostics::DiagnosticsHandle;
use super::super::error::RuntimeError;
use super::super::instance::{ValidationMode, VulkanInstance};

pub(in crate::backends::vulkan) struct WindowSurface {
    pub(in crate::backends::vulkan) instance: Arc<VulkanInstance>,
    pub(in crate::backends::vulkan) loader: ash::khr::surface::Instance,
    pub(in crate::backends::vulkan) handle: vk::SurfaceKHR,
    // The native Surface is destroyed before this last Window owner is dropped.
    _window: Arc<Window>,
}

impl WindowSurface {
    pub(in crate::backends::vulkan) fn new(
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
        let handle = window.window_handle().map_err(|error| {
            RuntimeError::invalid(format!("Windows window handle unavailable: {error}"))
        })?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return Err(RuntimeError::unsupported("Win32 window handle is required"));
        };
        let hinstance = handle
            .hinstance
            .ok_or_else(|| RuntimeError::invalid("Windows instance handle unavailable"))?;
        let platform = ash::khr::win32_surface::Instance::new(instance.entry(), instance.raw());
        let info = vk::Win32SurfaceCreateInfoKHR::default()
            .hinstance(hinstance.get())
            .hwnd(handle.hwnd.get());
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
        // SAFETY: winit supplied the live HWND and its HINSTANCE. The owning
        // value retains the Window and Instance until after Surface destruction;
        // the Win32 Surface extension was checked and enabled above.
        let surface = unsafe { platform.create_win32_surface(&info, None) }.map_err(|result| {
            RuntimeError::native("vkCreateWin32SurfaceKHR", "pre-device", result)
        })?;
        let loader = ash::khr::surface::Instance::new(instance.entry(), instance.raw());
        Ok(Arc::new(Self {
            instance,
            loader,
            handle: surface,
            _window: window,
        }))
    }

    pub(in crate::backends::vulkan) fn assess_device(
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
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectDestroy, 1);
        // SAFETY: Swapchain/work owners have released this final Surface owner.
        unsafe { self.loader.destroy_surface(self.handle, None) };
    }
}

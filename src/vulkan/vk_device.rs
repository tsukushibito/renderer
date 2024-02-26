use std::rc::Rc;

use ash::{vk, Device, Instance};

use super::super::Result;
use super::{vk_physical_device::VkPhysicalDevice, VkRenderBackend};

pub struct VkDevice {
    pub(crate) device: Device,
    pub(crate) swapchain_loader: ash::extensions::khr::Swapchain,
}

impl VkDevice {
    pub fn new(
        backend: &VkRenderBackend,
        physical_device: &VkPhysicalDevice,
    ) -> Result<Rc<VkDevice>> {
        let device = create_device(backend, physical_device)?;
        let swapchain_loader = ash::extensions::khr::Swapchain::new(&backend.instance, &device);

        Ok(Rc::new(Self {
            device,
            swapchain_loader,
        }))
    }
}

fn create_device(backend: &VkRenderBackend, physical_device: &VkPhysicalDevice) -> Result<Device> {
    let extension_names = [
        ash::extensions::khr::Swapchain::name().as_ptr(),
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        vk::KhrPortabilitySubsetFn::name().as_ptr(),
    ];

    let queue_priorities = [1.0];
    let graphics_family_index = physical_device.graphics_family;
    let graphics_queue_create_info = vk::DeviceQueueCreateInfo::builder()
        .queue_family_index(graphics_family_index)
        .queue_priorities(&queue_priorities)
        .build();
    let mut queue_infos = vec![graphics_queue_create_info];

    let present_family_index = physical_device.present_family;
    if present_family_index != graphics_family_index {
        let present_queue_create_info = vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(physical_device.present_family)
            .queue_priorities(&queue_priorities)
            .build();
        queue_infos.push(present_queue_create_info);
    }

    let create_info = vk::DeviceCreateInfo::builder()
        .enabled_extension_names(&extension_names)
        .queue_create_infos(&queue_infos)
        .build();

    let device = unsafe {
        backend
            .instance
            .create_device(physical_device.physical_device, &create_info, None)?
    };
    Ok(device)
}

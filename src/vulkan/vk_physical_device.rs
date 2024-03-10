use std::rc::Rc;

use ash::{vk, Instance};

use crate::{DeviceType, PhysicalDevice};

pub struct VkPhysicalDevice {
    pub(crate) physical_device: vk::PhysicalDevice,
    pub(crate) properties: vk::PhysicalDeviceProperties,
    pub(crate) memory_properties: vk::PhysicalDeviceMemoryProperties,
    pub(crate) graphics_family: u32,
    pub(crate) present_family: u32,
}

impl VkPhysicalDevice {
    pub(crate) fn new(
        instance: &Instance,
        physical_device: vk::PhysicalDevice,
    ) -> VkPhysicalDevice {
        let properties = unsafe { instance.get_physical_device_properties(physical_device) };
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };
        Self {
            physical_device,
            properties,
            memory_properties,
            graphics_family: 0,
            present_family: 0,
        }
    }
}

impl PhysicalDevice for VkPhysicalDevice {
    fn device_type(&self) -> DeviceType {
        match self.properties.device_type {
            vk::PhysicalDeviceType::INTEGRATED_GPU => DeviceType::Integrated,
            vk::PhysicalDeviceType::DISCRETE_GPU => DeviceType::Discrete,
            _ => DeviceType::Other,
        }
    }

    fn vram_size(&self) -> usize {
        let mut size: usize = 0;
        for i in 0..self.memory_properties.memory_heap_count {
            let heap = self.memory_properties.memory_heaps[i as usize];
            if heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL) {
                size = heap.size as usize;
            }
        }

        size
    }
}

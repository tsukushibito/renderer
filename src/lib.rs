use std::sync::Arc;

pub mod vulkan;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub trait RenderBackend {
    type PhysDev: PhysicalDevice;
    fn physical_devices(&self) -> &Vec<Arc<Self::PhysDev>>;
}

pub enum DeviceType {
    Integrated,
    Discrete,
    Other,
}

pub trait PhysicalDevice {
    fn device_type(&self) -> DeviceType;
    fn vram_size(&self) -> usize;
}

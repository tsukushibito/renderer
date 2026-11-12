//! VMA and its Vulkan dependencies share one lifetime, retained by every resource.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use ash::vk;

use super::device::VulkanDevice;
use super::instance::{LOG_TARGET, REQUIRED_API};

#[derive(Debug)]
pub(super) struct AllocatorInitError {
    result: vk::Result,
}

impl fmt::Display for AllocatorInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vmaCreateAllocator failed: {:?}", self.result)
    }
}

impl Error for AllocatorInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.result)
    }
}

/// Field order is part of the lifetime contract: VMA is destroyed before Device.
/// Buffers and images retain an Arc rather than only borrowing this owner.
/// Forgetting a resource therefore also leaks its owner instead of destroying
/// VMA underneath it.
pub(super) struct MemoryOwner {
    pub(super) raw: vk_mem::Allocator,
    pub(super) device: VulkanDevice,
}

pub(super) struct VulkanAllocator {
    pub(super) owner: Arc<MemoryOwner>,
}

impl VulkanAllocator {
    pub(super) fn new(device: VulkanDevice) -> Result<Self, AllocatorInitError> {
        let mut create_info = vk_mem::AllocatorCreateInfo::new(
            device.instance_raw(),
            device.raw(),
            device.physical_device(),
        );
        // Match the application's Instance API, not the adapter's newer API.
        create_info.vulkan_api_version = REQUIRED_API;
        // SAFETY: All three Vulkan objects belong to this Device owner. The
        // resulting MemoryOwner retains them and drops VMA before the Device.
        // Default flags preserve VMA's internal host synchronization.
        let raw = unsafe { vk_mem::Allocator::new(create_info) }.map_err(|result| {
            let error = AllocatorInitError { result };
            tracing::error!(target: LOG_TARGET, device = device.name(), error = %error,
                "Vulkan allocator initialization failed");
            error
        })?;
        let allocator = Self {
            owner: Arc::new(MemoryOwner { raw, device }),
        };
        tracing::info!(target: LOG_TARGET, device = allocator.owner.device.name(),
            device_type = ?allocator.owner.device.device_type(), vulkan_api = "1.3",
            "Vulkan memory allocator initialized");
        Ok(allocator)
    }
}

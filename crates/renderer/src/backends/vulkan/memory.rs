//! VMA and its Vulkan dependencies share one lifetime, retained by every resource.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use ash::vk;

use super::device::VulkanDevice;
use super::instance::{LOG_TARGET, REQUIRED_API};

#[cfg(feature = "memory-profiling")]
mod profiling;

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
        // Both an owned and a host-supplied instance support this 1.3 baseline.
        create_info.vulkan_api_version = REQUIRED_API;
        #[cfg(feature = "memory-profiling")]
        let host_callbacks = profiling::host_callbacks();
        #[cfg(feature = "memory-profiling")]
        let mut device_callbacks =
            create_info
                .device_memory_callbacks
                .copied()
                .unwrap_or_else(|| {
                    // SAFETY: The pinned VmaDeviceMemoryCallbacks contains only optional
                    // function pointers and a user pointer; all-null is its valid default.
                    unsafe { std::mem::zeroed() }
                });
        #[cfg(feature = "memory-profiling")]
        {
            device_callbacks.pfnAllocate = Some(profiling::device_allocate::<_>);
            device_callbacks.pfnFree = Some(profiling::device_free::<_>);
            create_info.allocation_callbacks = Some(&host_callbacks);
            create_info.device_memory_callbacks = Some(&device_callbacks);
        }
        // SAFETY: All three Vulkan objects belong to this Device owner. The
        // resulting MemoryOwner retains them and drops VMA before the Device.
        // Default flags preserve VMA's internal host synchronization.
        // VMA copies both callback structs; their function pointers are static
        // and user data is null, so none borrow these temporary stack values.
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

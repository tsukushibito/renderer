//! Device-only graph buffers. Pending graph owners retain this allocation.

#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::CString;
use std::sync::Arc;

use ash::vk;
use vk_mem::Alloc;

use super::super::memory::MemoryOwner;
use super::RuntimeError;

/// Owns the buffer and allocation together. Initialization and scheduled access
/// belong to the graph runtime, independently of this physical allocation.
pub(super) struct GraphBuffer {
    pub(super) owner: Arc<MemoryOwner>,
    pub(super) handle: vk::Buffer,
    pub(super) size: u64,
    allocation: vk_mem::Allocation,
}

fn create_buffer<T>(
    size: u64,
    max_buffer_size: u64,
    create: impl FnOnce(
        &vk::BufferCreateInfo<'_>,
        &vk_mem::AllocationCreateInfo,
    ) -> Result<T, RuntimeError>,
) -> Result<T, RuntimeError> {
    if size == 0 || size > max_buffer_size {
        return Err(RuntimeError::invalid(format!(
            "graph buffer size {size} must be nonzero and not exceed Device maxBufferSize {max_buffer_size}"
        )));
    }
    let info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::VERTEX_BUFFER
                | vk::BufferUsageFlags::INDEX_BUFFER,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let allocation = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    create(&info, &allocation)
}

impl GraphBuffer {
    pub(super) fn new(
        owner: Arc<MemoryOwner>,
        size: u64,
        #[cfg_attr(not(feature = "vulkan-diagnostics"), allow(unused_variables))] name: &str,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        #[cfg(feature = "vulkan-diagnostics")]
        let name = CString::new(name)
            .map_err(|_| RuntimeError::invalid("graph buffer debug name contains a NUL byte"))?;
        let (handle, allocation) =
            create_buffer(size, owner.device.max_buffer_size(), |info, allocation| {
                // SAFETY: VMA and its Device are retained by owner. This request
                // creates an unaliased, device-only buffer with no escaped mapping.
                let result = unsafe { owner.raw.create_buffer(info, allocation) };
                if let Err(result) = result {
                    owner.device.observe_result(result);
                }
                result.map_err(|result| {
                    RuntimeError::native(
                        format!("vmaCreateBuffer for graph buffer ({size} bytes)"),
                        owner.device.name(),
                        result,
                    )
                })
            })?;
        // Own successful native creation before the fallible naming operation.
        let buffer = Self {
            owner,
            handle,
            size,
            allocation,
        };
        #[cfg(feature = "vulkan-diagnostics")]
        {
            // SAFETY: The uniquely owned buffer belongs to this live Device. The
            // CString remains valid through this synchronous naming operation.
            unsafe {
                buffer
                    .owner
                    .device
                    .debug_utils()
                    .set_debug_utils_object_name(
                        &vk::DebugUtilsObjectNameInfoEXT::default()
                            .object_handle(handle)
                            .object_name(&name),
                    )
            }
            .map_err(|result| {
                buffer.owner.device.observe_result(result);
                RuntimeError::native(
                    "vkSetDebugUtilsObjectNameEXT for graph buffer",
                    buffer.owner.device.name(),
                    result,
                )
            })?;
        }
        Ok(buffer)
    }
}

impl Drop for GraphBuffer {
    fn drop(&mut self) {
        // SAFETY: Graph recording/pending owners retain the physical allocation.
        // Destruction is reached only before submit or after safe retirement;
        // unresolved submissions retain their complete ownership chain instead.
        // VMA and Device remain live through this owner's Arc.
        unsafe {
            self.owner
                .raw
                .destroy_buffer(self.handle, &mut self.allocation)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_size_is_rejected_before_native_creation() {
        for size in [0, 97, u64::MAX] {
            let error =
                create_buffer::<()>(size, 96, |_, _| panic!("invalid size must not reach VMA"))
                    .unwrap_err();
            assert_eq!(error.kind(), super::super::RuntimeErrorKind::InvalidInput);
            assert!(error.message.contains("maxBufferSize 96"));
        }
    }

    #[test]
    fn graph_buffer_request_supports_actual_operations_and_preserves_failure() {
        let error = create_buffer::<()>(96, 96, |info, allocation| {
            assert_eq!(info.size, 96);
            assert_eq!(info.sharing_mode, vk::SharingMode::EXCLUSIVE);
            assert_eq!(
                info.usage,
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::VERTEX_BUFFER
                    | vk::BufferUsageFlags::INDEX_BUFFER
            );
            assert_eq!(allocation.usage, vk_mem::MemoryUsage::AutoPreferDevice);
            assert!(allocation.flags.is_empty());
            Err(RuntimeError::native(
                "vmaCreateBuffer for graph buffer (96 bytes)",
                "test device",
                vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), super::super::RuntimeErrorKind::Native);
        assert!(error.message.contains("vmaCreateBuffer"));
        assert!(error.message.contains("96 bytes"));
        assert!(error.message.contains("ERROR_OUT_OF_DEVICE_MEMORY"));
    }
}

//! Unique buffer/allocation ownership and bounded, copy-based CPU access.
//!
//! Transfer owners move buffers from recording through successful completion,
//! excluding CPU access during GPU use. The private raw-handle accessor requires
//! that ownership contract; only successful completion establishes GPU contents.

use std::error::Error;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use ash::vk;
use vk_mem::Alloc;

use super::device::DeviceState;
use super::instance::LOG_TARGET;
use super::memory::{MemoryOwner, VulkanAllocator};

mod host_access;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BufferRole {
    Upload,
    DeviceTransfer,
    Readback,
}

impl BufferRole {
    fn host_access(self, operation: &'static str) -> Result<(), BufferError> {
        let allowed = match operation {
            "write" => self != Self::DeviceTransfer,
            "read" => self == Self::Readback,
            _ => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(BufferError::AccessDenied {
                role: self,
                operation,
            })
        }
    }

    fn create_info(self) -> (vk::BufferUsageFlags, vk_mem::AllocationCreateInfo) {
        let (usage, memory_usage, flags) = match self {
            Self::Upload => (
                vk::BufferUsageFlags::TRANSFER_SRC,
                vk_mem::MemoryUsage::AutoPreferHost,
                vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            ),
            Self::DeviceTransfer => (
                vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST,
                vk_mem::MemoryUsage::AutoPreferDevice,
                vk_mem::AllocationCreateFlags::empty(),
            ),
            Self::Readback => (
                vk::BufferUsageFlags::TRANSFER_DST,
                vk_mem::MemoryUsage::AutoPreferHost,
                vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM,
            ),
        };
        (
            usage,
            vk_mem::AllocationCreateInfo {
                usage: memory_usage,
                flags,
                ..Default::default()
            },
        )
    }
}

#[derive(Debug)]
pub(super) enum BufferError {
    DeviceUnavailable(DeviceState),
    InvalidSize {
        role: BufferRole,
        size: vk::DeviceSize,
        max_buffer_size: vk::DeviceSize,
    },
    OutOfBounds {
        role: BufferRole,
        size: vk::DeviceSize,
        offset: vk::DeviceSize,
        len: usize,
    },
    AccessDenied {
        role: BufferRole,
        operation: &'static str,
    },
    Uninitialized {
        offset: vk::DeviceSize,
        len: usize,
    },
    Vulkan {
        operation: &'static str,
        role: BufferRole,
        size: vk::DeviceSize,
        offset: vk::DeviceSize,
        len: vk::DeviceSize,
        result: vk::Result,
    },
}

impl fmt::Display for BufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceUnavailable(state) => {
                write!(f, "buffer Device is {state:?}; normal use is stopped")
            }
            Self::InvalidSize {
                role,
                size,
                max_buffer_size,
            } => {
                write!(
                    f,
                    "invalid {role:?} buffer size {size}; Device maxBufferSize is {max_buffer_size}"
                )
            }
            Self::OutOfBounds {
                role,
                size,
                offset,
                len,
            } => write!(
                f,
                "{role:?} buffer size {size} cannot access offset {offset}, length {len}"
            ),
            Self::AccessDenied { role, operation } => {
                write!(f, "{role:?} buffer does not permit CPU {operation}")
            }
            Self::Uninitialized { offset, len } => write!(
                f,
                "readback range offset {offset}, length {len} has no established contents"
            ),
            Self::Vulkan {
                operation,
                role,
                size,
                offset,
                len,
                result,
            } => write!(
                f,
                "{operation} for {role:?} buffer size {size}, offset {offset}, length {len} failed: {result:?}"
            ),
        }
    }
}

impl Error for BufferError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Vulkan { result, .. } => Some(result),
            _ => None,
        }
    }
}

fn valid_size(
    role: BufferRole,
    size: vk::DeviceSize,
    max_buffer_size: vk::DeviceSize,
) -> Result<(), BufferError> {
    if size == 0
        || size > max_buffer_size
        || (role != BufferRole::DeviceTransfer && size > isize::MAX as u64)
    {
        Err(BufferError::InvalidSize {
            role,
            size,
            max_buffer_size,
        })
    } else {
        Ok(())
    }
}

fn checked_range(
    role: BufferRole,
    size: vk::DeviceSize,
    offset: vk::DeviceSize,
    len: usize,
) -> Result<Range<usize>, BufferError> {
    let invalid = || BufferError::OutOfBounds {
        role,
        size,
        offset,
        len,
    };
    let len64 = u64::try_from(len).map_err(|_| invalid())?;
    let end = offset.checked_add(len64).ok_or_else(invalid)?;
    if end > size || end > isize::MAX as u64 {
        return Err(invalid());
    }
    Ok(usize::try_from(offset).map_err(|_| invalid())?
        ..usize::try_from(end).map_err(|_| invalid())?)
}

/// Initialization and hazard tracking share the same ordered byte-range union.
pub(super) type InitializedRanges = super::ranges::ByteRanges;

/// Construct the actual request through one narrow boundary. This also allows a
/// creation error to be tested without inventing a VMA allocation or fake driver.
fn create_resource(
    role: BufferRole,
    size: vk::DeviceSize,
    max_buffer_size: vk::DeviceSize,
    create: impl FnOnce(
        &vk::BufferCreateInfo<'_>,
        &vk_mem::AllocationCreateInfo,
    ) -> Result<(vk::Buffer, vk_mem::Allocation), vk::Result>,
) -> Result<(vk::Buffer, vk_mem::Allocation), BufferError> {
    create_resource_with_flags(
        role,
        size,
        max_buffer_size,
        vk_mem::AllocationCreateFlags::empty(),
        create,
    )
}

fn create_resource_with_flags(
    role: BufferRole,
    size: vk::DeviceSize,
    max_buffer_size: vk::DeviceSize,
    extra_flags: vk_mem::AllocationCreateFlags,
    create: impl FnOnce(
        &vk::BufferCreateInfo<'_>,
        &vk_mem::AllocationCreateInfo,
    ) -> Result<(vk::Buffer, vk_mem::Allocation), vk::Result>,
) -> Result<(vk::Buffer, vk_mem::Allocation), BufferError> {
    valid_size(role, size, max_buffer_size)?;
    let (usage, mut allocation_info) = role.create_info();
    allocation_info.flags |= extra_flags;
    let buffer_info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    create(&buffer_info, &allocation_info).map_err(|result| BufferError::Vulkan {
        operation: "vmaCreateBuffer",
        role,
        size,
        offset: 0,
        len: size,
        result,
    })
}

/// Owns exactly one buffer/allocation pair; it cannot be cloned or split.
pub(super) struct VulkanBuffer {
    owner: Arc<MemoryOwner>,
    buffer: vk::Buffer,
    allocation: vk_mem::Allocation,
    role: BufferRole,
    size: vk::DeviceSize,
    memory_type: u32,
    memory_properties: vk::MemoryPropertyFlags,
    initialized: InitializedRanges,
}

impl VulkanAllocator {
    pub(super) fn create_buffer(
        &self,
        role: BufferRole,
        size: vk::DeviceSize,
    ) -> Result<VulkanBuffer, BufferError> {
        self.create_buffer_with_flags(role, size, vk_mem::AllocationCreateFlags::empty())
    }

    /// Shared Upload buffers keep VMA's native mapping pinned. Individual CPU
    /// copies still use balanced VMA map references and retain the write API.
    pub(super) fn create_staging_buffer(
        &self,
        role: BufferRole,
        size: vk::DeviceSize,
    ) -> Result<VulkanBuffer, BufferError> {
        let flags = if role == BufferRole::Upload {
            vk_mem::AllocationCreateFlags::MAPPED
        } else {
            vk_mem::AllocationCreateFlags::empty()
        };
        self.create_buffer_with_flags(role, size, flags)
    }

    fn create_buffer_with_flags(
        &self,
        role: BufferRole,
        size: vk::DeviceSize,
        flags: vk_mem::AllocationCreateFlags,
    ) -> Result<VulkanBuffer, BufferError> {
        let result = (|| {
            self.owner
                .device
                .ensure_active()
                .map_err(BufferError::DeviceUnavailable)?;
            let (buffer, allocation) = create_resource_with_flags(
                role,
                size,
                self.owner.device.max_buffer_size(),
                flags,
                |buffer_info, allocation_info| {
                    // SAFETY: The live allocator owns its Vulkan dependencies. The
                    // request creates a uniquely owned, unaliased transfer buffer.
                    unsafe { self.owner.raw.create_buffer(buffer_info, allocation_info) }
                },
            )?;
            // Own successful native results before information queries or logging.
            let mut owner = VulkanBuffer {
                owner: Arc::clone(&self.owner),
                buffer,
                allocation,
                role,
                size,
                memory_type: 0,
                memory_properties: vk::MemoryPropertyFlags::empty(),
                initialized: InitializedRanges::default(),
            };
            let info = owner.owner.raw.get_allocation_info(&owner.allocation);
            // SAFETY: VMA retains this immutable memory-properties table for the
            // whole live allocator lifetime, retained by the buffer's Arc.
            let properties = unsafe { owner.owner.raw.get_memory_properties() };
            owner.memory_type = info.memory_type;
            owner.memory_properties =
                properties.memory_types[info.memory_type as usize].property_flags;
            let name = match role {
                BufferRole::Upload => c"renderer.upload",
                BufferRole::DeviceTransfer => c"renderer.device-transfer",
                BufferRole::Readback => c"renderer.readback",
            };
            // SAFETY: The unique live buffer belongs to this Device, debug_utils
            // is enabled, and the borrowed name remains live through the call.
            unsafe {
                owner
                    .owner
                    .device
                    .debug_utils()
                    .set_debug_utils_object_name(
                        &vk::DebugUtilsObjectNameInfoEXT::default()
                            .object_handle(buffer)
                            .object_name(name),
                    )
            }
            .map_err(|result| {
                owner.owner.device.observe_result(result);
                BufferError::Vulkan {
                    operation: "vkSetDebugUtilsObjectNameEXT",
                    role,
                    size,
                    offset: 0,
                    len: size,
                    result,
                }
            })?;
            tracing::info!(target: LOG_TARGET, buffer = ?owner.buffer, role = ?role,
                size, allocation_size = info.size, memory_type = owner.memory_type,
                memory_properties = ?owner.memory_properties,
                "Vulkan buffer allocated");
            Ok(owner)
        })();
        if let Err(BufferError::Vulkan { result, .. }) = &result {
            self.owner.device.observe_result(*result);
        }
        if let Err(error) = &result {
            tracing::error!(target: LOG_TARGET, role = ?role, size, error = %error,
                "Vulkan buffer creation failed");
        }
        result
    }
}

impl VulkanBuffer {
    pub(super) fn owner(&self) -> &Arc<MemoryOwner> {
        &self.owner
    }

    pub(super) fn role(&self) -> BufferRole {
        self.role
    }

    pub(super) fn size(&self) -> vk::DeviceSize {
        self.size
    }

    pub(super) fn fully_initialized(&self) -> bool {
        self.initialized.contains(&(0..self.size))
    }

    pub(super) fn initialized_ranges(&self) -> InitializedRanges {
        self.initialized.clone()
    }

    /// # Safety
    /// The caller must retain exclusive ownership from command recording through
    /// successful completion or lost-device retirement; no concurrent CPU access
    /// or native escape beyond that owner is allowed.
    pub(super) unsafe fn transfer_handle(&self) -> vk::Buffer {
        self.buffer
    }

    /// # Safety
    /// A range owner retains this allocation through every recorded GPU use.
    /// Its copy ranges and their non-coherent cache atoms are disjoint from
    /// every concurrent CPU access and conflicting GPU use. The shared staging
    /// owner serializes all CPU mapping/cache operations and never resizes.
    pub(super) unsafe fn shared_transfer_handle(&self) -> vk::Buffer {
        self.buffer
    }

    /// # Safety
    /// The caller has proved successful completion of a full-buffer GPU write.
    pub(super) unsafe fn establish_gpu_contents(&mut self) {
        self.initialized.insert(0..self.size);
    }

    /// # Safety
    /// The caller proved successful completion of a GPU write to this exact,
    /// in-bounds range on an active Device. No failed/unfinished write is valid.
    pub(super) unsafe fn establish_gpu_range(&mut self, offset: u64, size: u64) {
        let end = offset.checked_add(size).expect("validated GPU write range");
        assert!(end <= self.size, "validated GPU write range is in bounds");
        self.initialized.insert(offset..end);
    }

    /// # Safety
    /// The caller exclusively owns this byte range and every cache atom touched
    /// by its flush. Those atoms have no pending GPU use or concurrent CPU
    /// access. Other atom-disjoint ranges may remain in GPU use. CPU mapping
    /// and cache calls on the allocation are externally serialized. This method
    /// does not establish whole-buffer initialization; the range owner tracks it.
    pub(super) unsafe fn write_shared_range(
        &mut self,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), BufferError> {
        self.owner
            .device
            .ensure_active()
            .map_err(BufferError::DeviceUnavailable)?;
        self.role.host_access("write")?;
        let range = checked_range(self.role, self.size, offset, bytes.len())?;
        let mut access = host_access::VmaAccess::new(&self.owner.raw, &mut self.allocation);
        // SAFETY: The caller's range/atom ownership excludes GPU conflicts and
        // checked_range established addressable, allocation-relative bounds.
        unsafe {
            host_access::write(
                &mut access,
                self.memory_properties
                    .contains(vk::MemoryPropertyFlags::HOST_COHERENT),
                range.start,
                bytes,
            )
        }
        .map_err(|error| {
            self.owner.device.observe_result(error.result);
            self.access_error(error, offset, bytes.len())
        })
    }

    /// # Safety
    /// The caller proved readable contents and exclusively owns this range and
    /// every cache atom touched by invalidate. No GPU operation can still write
    /// those atoms. Mapping/cache operations are externally serialized; other
    /// atom-disjoint ranges may remain in GPU use. Initialization is tracked by
    /// the range lease rather than this buffer's whole-owner bookkeeping.
    pub(super) unsafe fn read_shared_range(
        &mut self,
        offset: u64,
        output: &mut [u8],
    ) -> Result<(), BufferError> {
        self.owner
            .device
            .ensure_active()
            .map_err(BufferError::DeviceUnavailable)?;
        self.role.host_access("read")?;
        let range = checked_range(self.role, self.size, offset, output.len())?;
        let mut access = host_access::VmaAccess::new(&self.owner.raw, &mut self.allocation);
        // SAFETY: The caller supplies initialized, retired range/cache atoms;
        // checked_range supplies addressable bounds and no mapped pointer escapes.
        unsafe {
            host_access::read(
                &mut access,
                self.memory_properties
                    .contains(vk::MemoryPropertyFlags::HOST_COHERENT),
                range.start,
                output,
            )
        }
        .map_err(|error| {
            self.owner.device.observe_result(error.result);
            self.access_error(error, offset, output.len())
        })
    }

    pub(super) fn write(
        &mut self,
        offset: vk::DeviceSize,
        bytes: &[u8],
    ) -> Result<(), BufferError> {
        let result = (|| {
            self.owner
                .device
                .ensure_active()
                .map_err(BufferError::DeviceUnavailable)?;
            self.role.host_access("write")?;
            let range = checked_range(self.role, self.size, offset, bytes.len())?;
            if range.is_empty() {
                return Ok(());
            }
            let mut access = host_access::VmaAccess::new(&self.owner.raw, &mut self.allocation);
            // SAFETY: The host role requested mappable memory, and the checked
            // range lies within the unique live allocation and is addressable.
            // Transfer owners move this buffer away during any GPU use.
            let copied = unsafe {
                host_access::write(
                    &mut access,
                    self.memory_properties
                        .contains(vk::MemoryPropertyFlags::HOST_COHERENT),
                    range.start,
                    bytes,
                )
            };
            if let Err(error) = copied {
                self.owner.device.observe_result(error.result);
                // A failed cache operation does not restore old contents. Stop
                // assuming any previous initialization until a write succeeds.
                self.initialized.clear();
                return Err(self.access_error(error, offset, bytes.len()));
            }
            self.initialized.insert(offset..offset + bytes.len() as u64);
            Ok(())
        })();
        self.log_access_error("write", &result);
        result
    }

    pub(super) fn read(
        &mut self,
        offset: vk::DeviceSize,
        output: &mut [u8],
    ) -> Result<(), BufferError> {
        let result = (|| {
            self.owner
                .device
                .ensure_active()
                .map_err(BufferError::DeviceUnavailable)?;
            self.role.host_access("read")?;
            let range = checked_range(self.role, self.size, offset, output.len())?;
            if !self
                .initialized
                .contains(&(offset..offset + output.len() as u64))
            {
                return Err(BufferError::Uninitialized {
                    offset,
                    len: output.len(),
                });
            }
            if range.is_empty() {
                return Ok(());
            }
            let mut access = host_access::VmaAccess::new(&self.owner.raw, &mut self.allocation);
            // SAFETY: Checked range and host-visible allocation as in write.
            // The initialized-range check also establishes readable contents;
            // no transfer owner can simultaneously own/use this buffer.
            unsafe {
                host_access::read(
                    &mut access,
                    self.memory_properties
                        .contains(vk::MemoryPropertyFlags::HOST_COHERENT),
                    range.start,
                    output,
                )
            }
            .map_err(|error| {
                self.owner.device.observe_result(error.result);
                self.access_error(error, offset, output.len())
            })
        })();
        self.log_access_error("read", &result);
        result
    }

    fn access_error(
        &self,
        error: host_access::AccessError,
        offset: vk::DeviceSize,
        len: usize,
    ) -> BufferError {
        BufferError::Vulkan {
            operation: error.operation,
            role: self.role,
            size: self.size,
            offset,
            len: len as u64,
            result: error.result,
        }
    }

    fn log_access_error(&self, operation: &str, result: &Result<(), BufferError>) {
        if let Err(error) = result {
            tracing::error!(target: LOG_TARGET, buffer = ?self.buffer, role = ?self.role,
                operation, size = self.size, error = %error, "Vulkan buffer CPU access failed");
        }
    }
}

impl Drop for VulkanBuffer {
    fn drop(&mut self) {
        // SAFETY: This owner uniquely owns the pair; all scoped mappings have
        // ended and any recording/pending transfer owner has retired before
        // returning the buffer or allowing this destruction.
        // Its Arc keeps VMA and Device alive throughout this destruction.
        unsafe {
            self.owner
                .raw
                .destroy_buffer(self.buffer, &mut self.allocation)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::super::device::VulkanDevice;
    use super::super::diagnostics::Diagnostics;
    use super::super::instance::VulkanInstance;
    use super::*;

    #[test]
    fn ranges_reject_overflow_and_bounds_and_accept_empty_at_end() {
        let role = BufferRole::Readback;
        assert_eq!(checked_range(role, 8, 3, 5).unwrap(), 3..8);
        assert_eq!(checked_range(role, 8, 8, 0).unwrap(), 8..8);
        for (size, offset, len) in [(8, 8, 1), (8, 9, 0), (u64::MAX, u64::MAX, 1)] {
            assert!(matches!(
                checked_range(role, size, offset, len),
                Err(BufferError::OutOfBounds { .. })
            ));
        }
        assert!(valid_size(role, 0, u64::MAX).is_err());
        assert!(valid_size(role, isize::MAX as u64 + 1, u64::MAX).is_err());
        assert!(valid_size(BufferRole::DeviceTransfer, isize::MAX as u64 + 1, u64::MAX).is_ok());
    }

    #[test]
    fn initialized_ranges_preserve_holes_and_merge_bridging_writes() {
        let mut initialized = InitializedRanges::default();
        assert!(!initialized.contains(&(0..1)));
        assert!(initialized.contains(&(8..8)));
        initialized.insert(6..10);
        initialized.insert(0..2);
        initialized.insert(12..16);
        assert!(initialized.contains(&(7..9)));
        assert!(!initialized.contains(&(1..7)));
        initialized.insert(2..6);
        assert!(initialized.contains(&(0..10)));
        assert!(!initialized.contains(&(0..16)));
        initialized.insert(8..14);
        assert!(initialized.contains(&(0..16)));
        initialized.insert(4..5);
        initialized.insert(20..20);
        assert!(initialized.contains(&(0..16)));
        assert!(!initialized.contains(&(0..17)));
        assert!(!initialized.overlaps(&(16..21)));
    }

    #[test]
    fn role_policy_rejects_cpu_access_even_if_device_memory_is_host_visible() {
        assert!(BufferRole::Upload.host_access("write").is_ok());
        assert!(BufferRole::Upload.host_access("read").is_err());
        assert!(BufferRole::Readback.host_access("write").is_ok());
        assert!(BufferRole::Readback.host_access("read").is_ok());
        assert!(BufferRole::DeviceTransfer.host_access("write").is_err());
        assert!(BufferRole::DeviceTransfer.host_access("read").is_err());
    }

    #[test]
    fn device_buffer_limit_rejects_requests_before_vma_for_every_role() {
        for role in [
            BufferRole::Upload,
            BufferRole::DeviceTransfer,
            BufferRole::Readback,
        ] {
            for size in [0, 97, u64::MAX] {
                let error = create_resource(role, size, 96, |_, _| {
                    panic!("invalid device size must not reach VMA")
                })
                .unwrap_err();
                assert!(matches!(error, BufferError::InvalidSize {
                    size: rejected, max_buffer_size: 96, ..
                } if rejected == size));
            }
            assert!(valid_size(role, 96, 96).is_ok());
        }
    }

    #[test]
    fn creation_failure_retains_request_context_and_original_result() {
        for (role, expected_usage, expected_host_flag) in [
            (
                BufferRole::Upload,
                vk::BufferUsageFlags::TRANSFER_SRC,
                vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            ),
            (
                BufferRole::DeviceTransfer,
                vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST,
                vk_mem::AllocationCreateFlags::empty(),
            ),
            (
                BufferRole::Readback,
                vk::BufferUsageFlags::TRANSFER_DST,
                vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM,
            ),
        ] {
            let error = create_resource(role, 17, 17, |buffer, allocation| {
                assert_eq!(buffer.size, 17);
                assert_eq!(buffer.usage, expected_usage);
                assert_eq!(buffer.sharing_mode, vk::SharingMode::EXCLUSIVE);
                assert_eq!(allocation.flags.bits(), expected_host_flag.bits());
                Err(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
            })
            .unwrap_err();
            assert_eq!(
                error.source().unwrap().downcast_ref::<vk::Result>(),
                Some(&vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
            );
            let message = error.to_string();
            assert!(message.contains("vmaCreateBuffer"), "{message}");
            assert!(message.contains("17"), "{message}");
        }
        assert!(
            create_resource_with_flags(
                BufferRole::Upload,
                17,
                17,
                vk_mem::AllocationCreateFlags::MAPPED,
                |_, allocation| {
                    assert_eq!(
                        allocation.flags.bits(),
                        (vk_mem::AllocationCreateFlags::MAPPED
                            | vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE)
                            .bits()
                    );
                    Err(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
                }
            )
            .is_err()
        );
        assert!(
            create_resource(BufferRole::Upload, 0, 17, |_, _| {
                panic!("invalid input must not call VMA")
            })
            .is_err()
        );
    }

    #[test]
    #[ignore = "explicit real-GPU VMA buffer test; requires validation and debug_utils"]
    fn vulkan_buffer_smoke() {
        let dispatch = super::super::real_vulkan_dispatch();
        tracing::dispatcher::with_default(&dispatch, || {
            for iteration in 0..4 {
                let diagnostics = Diagnostics::new();
                let instance = VulkanInstance::new(Arc::clone(&diagnostics))
                    .unwrap_or_else(|error| panic!("iteration {iteration}: {error}"));
                super::super::assert_real_validation(&diagnostics);
                let device = VulkanDevice::new(instance)
                    .unwrap_or_else(|error| panic!("iteration {iteration}: {error}"));
                assert_ne!(
                    device.device_type(),
                    vk::PhysicalDeviceType::CPU,
                    "CPU fallback does not count as real-GPU acceptance"
                );
                let allocator = VulkanAllocator::new(device).unwrap();
                let maximum = allocator.owner.device.max_buffer_size();
                assert!(maximum > 0, "Vulkan 1.3 maxBufferSize must be available");
                if let Some(oversized) = maximum.checked_add(1) {
                    assert!(matches!(
                        allocator.create_buffer(BufferRole::DeviceTransfer, oversized),
                        Err(BufferError::InvalidSize { size, max_buffer_size, .. })
                            if size == oversized && max_buffer_size == maximum
                    ));
                }
                let lifetime = Arc::downgrade(&allocator.owner);
                let mut upload = allocator.create_buffer(BufferRole::Upload, 257).unwrap();
                let mut transfer = allocator
                    .create_buffer(BufferRole::DeviceTransfer, 257)
                    .unwrap();
                let mut readback = allocator.create_buffer(BufferRole::Readback, 257).unwrap();
                let pattern: Vec<_> = (0..257)
                    .map(|index| (index * 37 + iteration) as u8)
                    .collect();
                upload.write(0, &pattern).unwrap();
                assert!(matches!(
                    upload.read(0, &mut [0]),
                    Err(BufferError::AccessDenied { .. })
                ));
                assert!(matches!(
                    transfer.write(0, &[1]),
                    Err(BufferError::AccessDenied { .. })
                ));
                assert!(matches!(
                    transfer.read(0, &mut [0]),
                    Err(BufferError::AccessDenied { .. })
                ));

                let mut output = vec![0; 257];
                assert!(matches!(
                    readback.read(0, &mut output),
                    Err(BufferError::Uninitialized { .. })
                ));
                // Exercise disjoint partial writes, an uninitialized hole, then
                // adjacency merging and the full requested buffer boundary.
                readback.write(0, &pattern[..113]).unwrap();
                readback.write(117, &pattern[117..]).unwrap();
                assert!(matches!(
                    readback.read(0, &mut output),
                    Err(BufferError::Uninitialized { .. })
                ));
                readback.write(113, &pattern[113..117]).unwrap();
                readback.read(0, &mut output).unwrap();
                assert_eq!(output, pattern);
                readback.write(257, &[]).unwrap();
                readback.read(257, &mut []).unwrap();
                assert!(matches!(
                    readback.write(257, &[0]),
                    Err(BufferError::OutOfBounds { .. })
                ));
                assert!(matches!(
                    readback.read(u64::MAX, &mut [0]),
                    Err(BufferError::OutOfBounds { .. })
                ));
                let coherent = readback
                    .memory_properties
                    .contains(vk::MemoryPropertyFlags::HOST_COHERENT);
                tracing::info!(target: LOG_TARGET, iteration, coherent,
                    non_coherent_executed = !coherent,
                    "VMA CPU write/read checked; GPU copy was not submitted");
                // A dropped factory must not destroy dependencies still retained
                // by resources. The last buffer drop tears down the entire chain.
                drop(allocator);
                assert!(lifetime.upgrade().is_some());
                drop(upload);
                drop(transfer);
                readback.read(0, &mut output).unwrap();
                assert_eq!(output, pattern);
                drop(readback);
                assert!(lifetime.upgrade().is_none());
                let report = diagnostics.snapshot();
                assert!(
                    report.is_clean(),
                    "iteration {iteration}, including shutdown: {report:#?}"
                );
                tracing::info!(target: LOG_TARGET, iteration, warnings = report.warnings,
                    errors = report.errors, callback_failures = report.callback_failures,
                    "Vulkan buffer smoke including shutdown passed");
            }
        });
    }
}

//! Unique buffer/allocation ownership and bounded, copy-based CPU access.
//!
//! No buffer handle escapes this module and no GPU commands are submitted yet.
//! Submission support must retain buffers from recording through completion,
//! exclude CPU access while they are in use, and establish written contents only
//! after successful completion. Rust's mutable borrow alone cannot prove that.

use std::error::Error;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use ash::vk;
use vk_mem::Alloc;

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
    InvalidSize {
        role: BufferRole,
        size: vk::DeviceSize,
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
            Self::InvalidSize { role, size } => {
                write!(f, "invalid {role:?} buffer size {size}")
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

fn valid_size(role: BufferRole, size: vk::DeviceSize) -> Result<(), BufferError> {
    if size == 0 || (role != BufferRole::DeviceTransfer && size > isize::MAX as u64) {
        Err(BufferError::InvalidSize { role, size })
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

/// Sorted, disjoint initialized ranges; adjacent writes are merged as well.
#[derive(Default)]
struct InitializedRanges(Vec<Range<vk::DeviceSize>>);

impl InitializedRanges {
    fn contains(&self, range: &Range<vk::DeviceSize>) -> bool {
        range.is_empty()
            || self
                .0
                .iter()
                .any(|known| known.start <= range.start && known.end >= range.end)
    }

    fn insert(&mut self, mut range: Range<vk::DeviceSize>) {
        if range.is_empty() {
            return;
        }
        let first = self.0.partition_point(|known| known.end < range.start);
        let mut after = first;
        while after < self.0.len() && self.0[after].start <= range.end {
            range.start = range.start.min(self.0[after].start);
            range.end = range.end.max(self.0[after].end);
            after += 1;
        }
        self.0.splice(first..after, [range]);
    }
}

/// Construct the actual request through one narrow boundary. This also allows a
/// creation error to be tested without inventing a VMA allocation or fake driver.
fn create_resource(
    role: BufferRole,
    size: vk::DeviceSize,
    create: impl FnOnce(
        &vk::BufferCreateInfo<'_>,
        &vk_mem::AllocationCreateInfo,
    ) -> Result<(vk::Buffer, vk_mem::Allocation), vk::Result>,
) -> Result<(vk::Buffer, vk_mem::Allocation), BufferError> {
    valid_size(role, size)?;
    let (usage, allocation_info) = role.create_info();
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
        let result = (|| {
            let (buffer, allocation) =
                create_resource(role, size, |buffer_info, allocation_info| {
                    // SAFETY: The live allocator owns its Vulkan dependencies. The
                    // request creates a uniquely owned, unaliased transfer buffer.
                    unsafe { self.owner.raw.create_buffer(buffer_info, allocation_info) }
                })?;
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
            tracing::info!(target: LOG_TARGET, buffer = ?owner.buffer, role = ?role,
                size, allocation_size = info.size, memory_type = owner.memory_type,
                memory_properties = ?owner.memory_properties,
                "Vulkan buffer allocated");
            Ok(owner)
        })();
        if let Err(error) = &result {
            tracing::error!(target: LOG_TARGET, role = ?role, size, error = %error,
                "Vulkan buffer creation failed");
        }
        result
    }
}

impl VulkanBuffer {
    pub(super) fn write(
        &mut self,
        offset: vk::DeviceSize,
        bytes: &[u8],
    ) -> Result<(), BufferError> {
        let result = (|| {
            self.role.host_access("write")?;
            let range = checked_range(self.role, self.size, offset, bytes.len())?;
            if range.is_empty() {
                return Ok(());
            }
            let mut access = host_access::VmaAccess::new(&self.owner.raw, &mut self.allocation);
            // SAFETY: The host role requested mappable memory, and the checked
            // range lies within the unique live allocation and is addressable.
            // No native handle escapes and no GPU commands exist in this stage.
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
                // A failed cache operation does not restore old contents. Stop
                // assuming any previous initialization until a write succeeds.
                self.initialized.0.clear();
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
            // no GPU commands can race the copy or invalidate these contents.
            unsafe {
                host_access::read(
                    &mut access,
                    self.memory_properties
                        .contains(vk::MemoryPropertyFlags::HOST_COHERENT),
                    range.start,
                    output,
                )
            }
            .map_err(|error| self.access_error(error, offset, output.len()))
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
        // ended and no native buffer handle or GPU work exists outside here.
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
        assert!(valid_size(role, 0).is_err());
        assert!(valid_size(role, isize::MAX as u64 + 1).is_err());
        assert!(valid_size(BufferRole::DeviceTransfer, isize::MAX as u64 + 1).is_ok());
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
        assert_eq!(initialized.0.len(), 1);
        assert_eq!(initialized.0[0], 0..16);
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
            let error = create_resource(role, 17, |buffer, allocation| {
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
            create_resource(BufferRole::Upload, 0, |_, _| {
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
                let device = VulkanDevice::new(instance)
                    .unwrap_or_else(|error| panic!("iteration {iteration}: {error}"));
                assert_ne!(
                    device.device_type(),
                    vk::PhysicalDeviceType::CPU,
                    "CPU fallback does not count as real-GPU acceptance"
                );
                let allocator = VulkanAllocator::new(device).unwrap();
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

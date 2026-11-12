//! Fixed-capacity shared staging with VMA placement and unique range lifetimes.
//!
//! GPU owners retain range leases, not only native buffer handles. A lease is
//! returned to VMA only after GPU retirement and, for Readback, CPU consumption.

use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use ash::vk;

use super::super::buffer::{BufferError, BufferRole, VulkanBuffer};
use super::super::device::DeviceState;
use super::super::memory::{MemoryOwner, VulkanAllocator};

#[derive(Debug)]
pub(in crate::backends::vulkan) enum StagingError {
    Invalid(&'static str),
    DeviceUnavailable(DeviceState),
    CapacityExhausted {
        capacity: u64,
        requested: u64,
        reserved: u64,
    },
    LengthMismatch {
        expected: u64,
        actual: usize,
    },
    AccessDenied {
        role: BufferRole,
        operation: &'static str,
    },
    Uninitialized,
    Buffer(BufferError),
    Vulkan {
        operation: &'static str,
        result: vk::Result,
    },
}

impl fmt::Display for StagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid shared staging: {reason}"),
            Self::DeviceUnavailable(state) => write!(f, "staging Device is {state:?}"),
            Self::CapacityExhausted {
                capacity,
                requested,
                reserved,
            } => write!(
                f,
                "staging has no contiguous range for {requested} bytes (capacity {capacity}, reserved {reserved})"
            ),
            Self::LengthMismatch { expected, actual } => write!(
                f,
                "staging range requires {expected} bytes, received {actual}"
            ),
            Self::AccessDenied { role, operation } => {
                write!(f, "{role:?} staging range does not permit CPU {operation}")
            }
            Self::Uninitialized => write!(f, "readback range has no completed contents"),
            Self::Buffer(source) => fmt::Display::fmt(source, f),
            Self::Vulkan { operation, result } => write!(f, "{operation} failed: {result:?}"),
        }
    }
}

impl Error for StagingError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Buffer(source) => Some(source),
            Self::Vulkan { result, .. } => Some(result),
            _ => None,
        }
    }
}

impl From<BufferError> for StagingError {
    fn from(source: BufferError) -> Self {
        Self::Buffer(source)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::backends::vulkan) struct StagingStats {
    pub(in crate::backends::vulkan) capacity: u64,
    pub(in crate::backends::vulkan) alignment: u64,
    pub(in crate::backends::vulkan) used_bytes: u64,
    pub(in crate::backends::vulkan) reserved_bytes: u64,
    pub(in crate::backends::vulkan) live_ranges: usize,
    pub(in crate::backends::vulkan) high_water_reserved_bytes: u64,
    pub(in crate::backends::vulkan) capacity_exhaustions: usize,
    pub(in crate::backends::vulkan) allocations: usize,
    pub(in crate::backends::vulkan) releases: usize,
    pub(in crate::backends::vulkan) buffer_creates: usize,
}

struct Reservation {
    allocation: vk_mem::VirtualAllocation,
    offset: u64,
    size: u64,
    reserved: u64,
}

/// Placement is entirely delegated to VMA. Returning an allocation requires its
/// unique Reservation; this type implements no second free-space algorithm.
struct Arena {
    block: vk_mem::VirtualBlock,
    stats: StagingStats,
}

// SAFETY: This block uses no application allocation callbacks or user pointers.
// Moving its sole owner between threads does not access it concurrently. Every
// shared call is serialized by StagingOwner.arena, and virtual allocation tokens
// are used only with that same block under the lock.
unsafe impl Send for Arena {}

fn padded_size(size: u64, alignment: u64) -> Result<u64, StagingError> {
    if size == 0 || size > isize::MAX as u64 {
        return Err(StagingError::Invalid(
            "range size must be nonzero and CPU-addressable",
        ));
    }
    size.checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or(StagingError::Invalid(
            "range alignment overflows DeviceSize",
        ))
}

impl Arena {
    fn new(capacity: u64, alignment: u64) -> Result<Self, StagingError> {
        if alignment < 4 || !alignment.is_power_of_two() {
            return Err(StagingError::Invalid(
                "alignment must be a power of two of at least four",
            ));
        }
        if capacity == 0 || capacity > isize::MAX as u64 || !capacity.is_multiple_of(alignment) {
            return Err(StagingError::Invalid(
                "capacity must be CPU-addressable and a nonzero multiple of cache/copy alignment",
            ));
        }
        let block = vk_mem::VirtualBlock::new(vk_mem::VirtualBlockCreateInfo {
            size: capacity,
            ..Default::default()
        })
        .map_err(|result| StagingError::Vulkan {
            operation: "vmaCreateVirtualBlock",
            result,
        })?;
        Ok(Self {
            block,
            stats: StagingStats {
                capacity,
                alignment,
                used_bytes: 0,
                reserved_bytes: 0,
                live_ranges: 0,
                high_water_reserved_bytes: 0,
                capacity_exhaustions: 0,
                allocations: 0,
                releases: 0,
                buffer_creates: 1,
            },
        })
    }

    fn allocate(&mut self, size: u64) -> Result<Reservation, StagingError> {
        let reserved = padded_size(size, self.stats.alignment)?;
        // SAFETY: This uniquely accessed live block receives a nonzero size and
        // power-of-two alignment. The returned token stays with its unique lease.
        let allocation = unsafe {
            self.block.allocate(vk_mem::VirtualAllocationCreateInfo {
                size: reserved,
                alignment: self.stats.alignment,
                user_data: 0,
                flags: vk_mem::VirtualAllocationCreateFlags::empty(),
            })
        };
        let (allocation, offset) = match allocation {
            Ok(value) => value,
            Err(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY) => {
                // This result means virtual space exhaustion, not driver OOM.
                self.stats.capacity_exhaustions = self.stats.capacity_exhaustions.saturating_add(1);
                return Err(StagingError::CapacityExhausted {
                    capacity: self.stats.capacity,
                    requested: size,
                    reserved: self.stats.reserved_bytes,
                });
            }
            Err(result) => {
                return Err(StagingError::Vulkan {
                    operation: "vmaVirtualAllocate",
                    result,
                });
            }
        };
        self.stats.used_bytes += size;
        self.stats.reserved_bytes += reserved;
        self.stats.live_ranges += 1;
        self.stats.allocations = self.stats.allocations.saturating_add(1);
        self.stats.high_water_reserved_bytes = self
            .stats
            .high_water_reserved_bytes
            .max(self.stats.reserved_bytes);
        Ok(Reservation {
            allocation,
            offset,
            size,
            reserved,
        })
    }

    fn release(&mut self, mut reservation: Reservation) {
        // SAFETY: This unique token was allocated from this same block and is
        // returned once, after its last GPU/CPU owner allows its lease to drop.
        unsafe { self.block.free(&mut reservation.allocation) };
        self.stats.used_bytes -= reservation.size;
        self.stats.reserved_bytes -= reservation.reserved;
        self.stats.live_ranges -= 1;
        self.stats.releases = self.stats.releases.saturating_add(1);
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        // SAFETY: No range can outlive StagingOwner because it retains an Arc.
        // Clearing also makes construction/test unwinding safe before VMA's
        // destructor asserts that every virtual allocation was released.
        unsafe { self.block.clear() };
    }
}

struct StagingOwner {
    owner: Arc<MemoryOwner>,
    role: BufferRole,
    handle: vk::Buffer,
    // Keep placement/statistics independent from native mapping/cache calls.
    // A diagnostic callback may inspect stats without taking the CPU-access lock.
    buffer: Mutex<VulkanBuffer>,
    arena: Mutex<Arena>,
}

impl StagingOwner {
    fn lock<'a, T>(&self, mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                self.owner.device.stop();
                mutex.clear_poison();
                poisoned.into_inner()
            }
        }
    }

    fn active(&self) -> Result<(), StagingError> {
        self.owner
            .device
            .ensure_active()
            .map_err(StagingError::DeviceUnavailable)
    }
}

#[derive(Clone)]
pub(in crate::backends::vulkan) struct SharedStaging {
    state: Arc<StagingOwner>,
}

impl SharedStaging {
    pub(in crate::backends::vulkan) fn new(
        owner: Arc<MemoryOwner>,
        role: BufferRole,
        capacity: u64,
    ) -> Result<Self, StagingError> {
        if role == BufferRole::DeviceTransfer {
            return Err(StagingError::Invalid(
                "shared staging role must be Upload or Readback",
            ));
        }
        owner
            .device
            .ensure_active()
            .map_err(StagingError::DeviceUnavailable)?;
        // SAFETY: The live owner retains this physical device's Instance. This
        // immutable property query occurs only when creating the shared buffer.
        let properties = unsafe {
            owner
                .device
                .instance_raw()
                .get_physical_device_properties(owner.device.physical_device())
        };
        let alignment = properties.limits.non_coherent_atom_size.max(4);
        let arena = Arena::new(capacity, alignment)?;
        let factory = VulkanAllocator {
            owner: Arc::clone(&owner),
        };
        let buffer = factory.create_staging_buffer(role, capacity)?;
        // SAFETY: No range has been allocated or submitted yet. The shared owner
        // retains this buffer and only leases atom-disjoint validated ranges.
        let handle = unsafe { buffer.shared_transfer_handle() };
        Ok(Self {
            state: Arc::new(StagingOwner {
                owner,
                role,
                handle,
                buffer: Mutex::new(buffer),
                arena: Mutex::new(arena),
            }),
        })
    }

    pub(in crate::backends::vulkan) fn allocate(
        &self,
        size: u64,
    ) -> Result<StagingRange, StagingError> {
        self.state.active()?;
        let reservation = {
            let mut arena = self.state.lock(&self.state.arena);
            self.state.active()?;
            arena.allocate(size)?
        };
        Ok(StagingRange {
            owner: Arc::clone(&self.state),
            reservation: Some(reservation),
            initialized: false,
        })
    }

    pub(in crate::backends::vulkan) fn stats(&self) -> StagingStats {
        self.state.lock(&self.state.arena).stats
    }
}

/// No Clone: a range has one CPU/GPU owner until it can be returned to VMA.
pub(in crate::backends::vulkan) struct StagingRange {
    owner: Arc<StagingOwner>,
    reservation: Option<Reservation>,
    initialized: bool,
}

impl StagingRange {
    pub(in crate::backends::vulkan) fn role(&self) -> BufferRole {
        self.owner.role
    }
    pub(in crate::backends::vulkan) fn owner(&self) -> &Arc<MemoryOwner> {
        &self.owner.owner
    }
    pub(in crate::backends::vulkan) fn offset(&self) -> u64 {
        self.reservation.as_ref().unwrap().offset
    }
    pub(in crate::backends::vulkan) fn size(&self) -> u64 {
        self.reservation.as_ref().unwrap().size
    }
    pub(in crate::backends::vulkan) fn initialized(&self) -> bool {
        self.initialized
    }

    fn check_len(&self, len: usize) -> Result<(), StagingError> {
        if u64::try_from(len).ok() != Some(self.size()) {
            return Err(StagingError::LengthMismatch {
                expected: self.size(),
                actual: len,
            });
        }
        Ok(())
    }

    pub(in crate::backends::vulkan) fn write(&mut self, bytes: &[u8]) -> Result<(), StagingError> {
        self.owner.active()?;
        if self.role() != BufferRole::Upload {
            return Err(StagingError::AccessDenied {
                role: self.role(),
                operation: "write",
            });
        }
        self.check_len(bytes.len())?;
        let result = {
            let mut buffer = self.owner.lock(&self.owner.buffer);
            self.owner.active()?;
            // SAFETY: This unique CPU lease owns its padded cache atoms. GPU
            // owners can retain only other disjoint leases; the mutex serializes
            // mapping/cache host calls. No pointer escapes this copy operation.
            unsafe { buffer.write_shared_range(self.offset(), bytes) }
        };
        self.initialized = result.is_ok();
        result.map_err(StagingError::Buffer)
    }

    pub(in crate::backends::vulkan) fn read(
        &mut self,
        output: &mut [u8],
    ) -> Result<(), StagingError> {
        self.owner.active()?;
        if self.role() != BufferRole::Readback {
            return Err(StagingError::AccessDenied {
                role: self.role(),
                operation: "read",
            });
        }
        self.check_len(output.len())?;
        if !self.initialized {
            return Err(StagingError::Uninitialized);
        }
        let result = {
            let mut buffer = self.owner.lock(&self.owner.buffer);
            self.owner.active()?;
            // SAFETY: establish_contents requires actual GPU write completion.
            // This retained lease excludes recycling and conflicting operations
            // on its entire padded cache-atom range during CPU invalidation/read.
            unsafe { buffer.read_shared_range(self.offset(), output) }
        };
        result.map_err(StagingError::Buffer)
    }

    /// # Safety
    /// The caller retains this unique lease through all recorded/pending GPU
    /// access. Commands stay within offset..offset+size, use the correct role,
    /// and CPU accesses cannot overlap that GPU lifetime. No raw handle escapes
    /// the retained owner; other leases access only atom-disjoint ranges.
    pub(in crate::backends::vulkan) unsafe fn transfer_handle(&self) -> vk::Buffer {
        self.owner.handle
    }

    /// # Safety
    /// An actual successful Fence wait proved a write to every byte of this
    /// range, and the Device remains active. Timeout/loss never establishes data.
    pub(in crate::backends::vulkan) unsafe fn establish_contents(&mut self) {
        self.initialized = true;
    }
}

impl Drop for StagingRange {
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            self.owner.lock(&self.owner.arena).release(reservation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_ranges_enforce_capacity_and_reclaim_out_of_order() {
        let mut arena = Arena::new(1024, 256).unwrap();
        let first = arena.allocate(257).unwrap();
        let second = arena.allocate(1).unwrap();
        let third = arena.allocate(17).unwrap();
        assert_eq!(
            (first.reserved, second.reserved, third.reserved),
            (512, 256, 256)
        );
        let ranges = [&first, &second, &third];
        for (index, range) in ranges.iter().enumerate() {
            assert_eq!(range.offset % 256, 0);
            for other in &ranges[index + 1..] {
                assert!(
                    range.offset + range.reserved <= other.offset
                        || other.offset + other.reserved <= range.offset
                );
            }
        }
        assert!(matches!(
            arena.allocate(1),
            Err(StagingError::CapacityExhausted { .. })
        ));
        assert_eq!(
            (
                arena.stats.used_bytes,
                arena.stats.reserved_bytes,
                arena.stats.live_ranges
            ),
            (275, 1024, 3)
        );
        let hole = second.offset;
        arena.release(second);
        let replacement = arena.allocate(100).unwrap();
        assert_eq!(replacement.offset, hole);
        arena.release(first);
        arena.release(third);
        arena.release(replacement);
        assert_eq!(
            (
                arena.stats.used_bytes,
                arena.stats.reserved_bytes,
                arena.stats.live_ranges
            ),
            (0, 0, 0)
        );
        let whole = arena.allocate(1024).unwrap();
        assert_eq!(whole.offset, 0);
        arena.release(whole);
        assert_eq!(arena.stats.high_water_reserved_bytes, 1024);
        assert_eq!(arena.stats.capacity_exhaustions, 1);
        assert_eq!(arena.stats.allocations, arena.stats.releases);
    }

    #[test]
    fn invalid_requests_never_change_live_reservations() {
        for (capacity, alignment) in [(0, 256), (257, 256), (1024, 0), (1024, 3)] {
            assert!(Arena::new(capacity, alignment).is_err());
        }
        let mut arena = Arena::new(256, 256).unwrap();
        for size in [0, u64::MAX, isize::MAX as u64 + 1] {
            assert!(arena.allocate(size).is_err());
        }
        assert_eq!(arena.stats.live_ranges, 0);
        assert_eq!(arena.stats.reserved_bytes, 0);
        assert!(matches!(
            arena.allocate(257),
            Err(StagingError::CapacityExhausted { .. })
        ));
    }
}

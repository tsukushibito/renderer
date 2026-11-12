//! Scoped CPU copies over the small VMA mapping/cache boundary.

use ash::{prelude::VkResult, vk};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AccessError {
    pub operation: &'static str,
    pub result: vk::Result,
}

/// The allocation behind one exclusive CPU access operation.
///
/// # Safety
/// A successful map must return a non-null pointer to the allocation's first
/// byte, remaining valid until the matching unmap. Implementations must not
/// move or free that storage during cache operations. Unmap must not unwind.
pub(super) unsafe trait MemoryAccess {
    /// # Safety
    /// The allocation must be live and its mapping/cache host calls exclusively
    /// controlled by this access. The copied range and its cache atoms exclude
    /// GPU conflicts; other atom-disjoint ranges may remain in GPU use.
    unsafe fn map(&mut self) -> VkResult<*mut u8>;

    /// # Safety
    /// This must pair with one successful, still outstanding map.
    unsafe fn unmap(&mut self);

    fn flush(&mut self, offset: vk::DeviceSize, size: vk::DeviceSize) -> VkResult<()>;
    fn invalidate(&mut self, offset: vk::DeviceSize, size: vk::DeviceSize) -> VkResult<()>;
}

pub(super) struct VmaAccess<'a> {
    allocator: &'a vk_mem::Allocator,
    allocation: &'a mut vk_mem::Allocation,
}

impl<'a> VmaAccess<'a> {
    pub(super) fn new(
        allocator: &'a vk_mem::Allocator,
        allocation: &'a mut vk_mem::Allocation,
    ) -> Self {
        Self {
            allocator,
            allocation,
        }
    }
}

// SAFETY: VMA's successful map points to this allocation, not the enclosing
// memory block, and stays valid through the paired unmap. The buffer owner
// retains the allocator and allocation without defragmentation. Callers exclude
// GPU access to the copied range and its cache atoms; shared staging may use
// other atom-disjoint ranges concurrently.
unsafe impl MemoryAccess for VmaAccess<'_> {
    unsafe fn map(&mut self) -> VkResult<*mut u8> {
        // SAFETY: The caller supplies the live, exclusively accessed allocation.
        unsafe { self.allocator.map_memory(self.allocation) }
    }

    unsafe fn unmap(&mut self) {
        // SAFETY: The caller pairs exactly one successful VMA map here.
        unsafe { self.allocator.unmap_memory(self.allocation) };
    }

    fn flush(&mut self, offset: vk::DeviceSize, size: vk::DeviceSize) -> VkResult<()> {
        // Offsets are allocation-relative. VMA performs nonCoherentAtomSize
        // rounding and keeps suballocations from sharing a non-coherent atom.
        self.allocator
            .flush_allocation(self.allocation, offset, size)
    }

    fn invalidate(&mut self, offset: vk::DeviceSize, size: vk::DeviceSize) -> VkResult<()> {
        self.allocator
            .invalidate_allocation(self.allocation, offset, size)
    }
}

struct Mapping<'a, A: MemoryAccess> {
    access: &'a mut A,
    pointer: *mut u8,
}

impl<'a, A: MemoryAccess> Mapping<'a, A> {
    /// # Safety
    /// The allocation must satisfy `MemoryAccess::map`'s live-access condition.
    unsafe fn new(access: &'a mut A) -> Result<Self, AccessError> {
        // SAFETY: Forward the live, exclusive access supplied by the caller.
        let pointer = unsafe { access.map() }.map_err(|result| AccessError {
            operation: "vmaMapMemory",
            result,
        })?;
        // Establish the guard immediately after success, before any operation
        // that could return an error or unwind.
        Ok(Self { access, pointer })
    }
}

impl<A: MemoryAccess> Drop for Mapping<'_, A> {
    fn drop(&mut self) {
        // SAFETY: A Mapping exists only after one successful map, and uniquely
        // owns that mapping until this single Drop. No mapped pointer escapes.
        unsafe { self.access.unmap() };
    }
}

/// Copy bytes into the allocation, flushing before the scoped mapping ends.
///
/// # Safety
/// The allocation must be live, and this range plus cache atoms touched by a
/// flush must be free of GPU/concurrent CPU access. Mapping/cache host calls on
/// the allocation must be serialized. The caller validates the range against
/// the allocation/CPU address space and excludes overlap with `bytes`.
pub(super) unsafe fn write(
    access: &mut impl MemoryAccess,
    coherent: bool,
    offset: usize,
    bytes: &[u8],
) -> Result<(), AccessError> {
    if bytes.is_empty() {
        return Ok(());
    }
    // SAFETY: The caller supplies exclusive access to a live allocation.
    let mapped = unsafe { Mapping::new(access) }?;
    // SAFETY: The caller checked addressable bounds and non-overlap; successful
    // map storage remains valid through the guard. u8 requires no alignment.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.pointer.add(offset), bytes.len())
    };
    if !coherent {
        mapped
            .access
            .flush(offset as vk::DeviceSize, bytes.len() as vk::DeviceSize)
            .map_err(|result| AccessError {
                operation: "vmaFlushAllocation",
                result,
            })?;
    }
    Ok(())
}

/// Invalidate the allocation's cache before copying initialized bytes out.
///
/// # Safety
/// The allocation must be live, and this range plus cache atoms touched by an
/// invalidate must be free of GPU/concurrent CPU access. Mapping/cache host
/// calls must be serialized. The caller validates addressable bounds,
/// establishes every requested byte, and excludes overlap with `output`.
pub(super) unsafe fn read(
    access: &mut impl MemoryAccess,
    coherent: bool,
    offset: usize,
    output: &mut [u8],
) -> Result<(), AccessError> {
    if output.is_empty() {
        return Ok(());
    }
    // SAFETY: The caller supplies exclusive access to a live allocation.
    let mapped = unsafe { Mapping::new(access) }?;
    if !coherent {
        mapped
            .access
            .invalidate(offset as vk::DeviceSize, output.len() as vk::DeviceSize)
            .map_err(|result| AccessError {
                operation: "vmaInvalidateAllocation",
                result,
            })?;
    }
    // SAFETY: The caller checked initialized, addressable bounds and
    // non-overlap. Invalidation succeeded before this copy starts.
    unsafe {
        std::ptr::copy_nonoverlapping(
            mapped.pointer.add(offset),
            output.as_mut_ptr(),
            output.len(),
        )
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Operation {
        Map,
        Flush,
        Invalidate,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        Map,
        Flush(vk::DeviceSize, vk::DeviceSize),
        Invalidate(vk::DeviceSize, vk::DeviceSize),
        Unmap,
    }

    struct Memory {
        bytes: Vec<u8>,
        events: Vec<Event>,
        fail: Option<Operation>,
        flush_snapshot: Option<Vec<u8>>,
        invalidate_fill: Option<u8>,
    }

    impl Memory {
        fn new(fail: Option<Operation>) -> Self {
            Self {
                bytes: vec![0; 16],
                events: Vec::new(),
                fail,
                flush_snapshot: None,
                invalidate_fill: None,
            }
        }

        fn result(&self, operation: Operation) -> VkResult<()> {
            if self.fail == Some(operation) {
                Err(vk::Result::ERROR_MEMORY_MAP_FAILED)
            } else {
                Ok(())
            }
        }
    }

    // SAFETY: The initialized Vec is never resized or freed during mapping,
    // cache operations leave its address stable, and unmap cannot unwind.
    unsafe impl MemoryAccess for Memory {
        unsafe fn map(&mut self) -> VkResult<*mut u8> {
            self.events.push(Event::Map);
            self.result(Operation::Map)?;
            Ok(self.bytes.as_mut_ptr())
        }

        unsafe fn unmap(&mut self) {
            self.events.push(Event::Unmap);
        }

        fn flush(&mut self, offset: vk::DeviceSize, size: vk::DeviceSize) -> VkResult<()> {
            self.events.push(Event::Flush(offset, size));
            self.flush_snapshot = Some(self.bytes.clone());
            self.result(Operation::Flush)
        }

        fn invalidate(&mut self, offset: vk::DeviceSize, size: vk::DeviceSize) -> VkResult<()> {
            self.events.push(Event::Invalidate(offset, size));
            self.result(Operation::Invalidate)?;
            if let Some(fill) = self.invalidate_fill {
                self.bytes.fill(fill);
            }
            Ok(())
        }
    }

    #[test]
    fn copies_use_allocation_relative_ranges_and_cache_order() {
        for coherent in [true, false] {
            let mut memory = Memory::new(None);
            let input = [11, 22, 33];
            // SAFETY: A distinct input and an in-bounds range in the live Vec.
            unsafe { write(&mut memory, coherent, 3, &input) }.unwrap();
            assert_eq!(&memory.bytes[3..6], &input);
            if coherent {
                assert_eq!(memory.events, [Event::Map, Event::Unmap]);
                assert!(memory.flush_snapshot.is_none());
            } else {
                assert_eq!(
                    memory.events,
                    [Event::Map, Event::Flush(3, 3), Event::Unmap]
                );
                assert_eq!(&memory.flush_snapshot.as_ref().unwrap()[3..6], &input);
            }

            memory.events.clear();
            memory.invalidate_fill = Some(77);
            let mut output = [0; 3];
            // SAFETY: Distinct output, initialized in-bounds bytes, no GPU work.
            unsafe { read(&mut memory, coherent, 3, &mut output) }.unwrap();
            if coherent {
                assert_eq!(output, input);
                assert_eq!(memory.events, [Event::Map, Event::Unmap]);
            } else {
                assert_eq!(output, [77; 3]);
                assert_eq!(
                    memory.events,
                    [Event::Map, Event::Invalidate(3, 3), Event::Unmap]
                );
            }
        }
    }

    #[test]
    fn mapping_and_cache_failures_preserve_cause_and_release_successful_maps() {
        for (failure, operation, expected_events) in [
            (Operation::Map, "vmaMapMemory", vec![Event::Map]),
            (
                Operation::Flush,
                "vmaFlushAllocation",
                vec![Event::Map, Event::Flush(3, 2), Event::Unmap],
            ),
            (
                Operation::Invalidate,
                "vmaInvalidateAllocation",
                vec![Event::Map, Event::Invalidate(3, 2), Event::Unmap],
            ),
        ] {
            let mut memory = Memory::new(Some(failure));
            let mut output = [91; 2];
            let result = if failure == Operation::Invalidate {
                // SAFETY: Distinct output, initialized in-bounds bytes.
                unsafe { read(&mut memory, false, 3, &mut output) }
            } else {
                // SAFETY: Distinct input and an in-bounds range in the live Vec.
                unsafe { write(&mut memory, false, 3, &[1, 2]) }
            };
            assert_eq!(
                result,
                Err(AccessError {
                    operation,
                    result: vk::Result::ERROR_MEMORY_MAP_FAILED,
                })
            );
            assert_eq!(memory.events, expected_events);
            assert_eq!(output, [91; 2]);
        }
    }
}

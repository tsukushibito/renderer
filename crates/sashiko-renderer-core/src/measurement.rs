//! Optional cumulative allocation events, independent of validation.
//!
//! Install [`CountingAllocator`] around the application's global allocator to
//! observe Rust heap requests. Backend hooks separately observe Renderer-issued
//! native calls and requests through VMA's host callbacks. Those callbacks may
//! also be forwarded to Vulkan, so their requests are not attributed separately
//! to VMA and the driver. Driver allocations outside the callbacks are not
//! observed. Snapshots and subtraction allocate nothing; format reports only
//! after the measured owners have been returned or dropped.
//! VMA callbacks should forward directly to a native allocator or `System`,
//! rather than the counting global allocator, to avoid counting the same host
//! allocation in both the Rust and VMA rows. Native event categories describe
//! separate requests, not one additive count of physical allocations.

use sashiko_heap::HeapSnapshot;
pub use sashiko_heap::{CountingAllocator, EventCount, RustEvent};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Requests through the host callbacks supplied to VMA, including callbacks
/// forwarded to Vulkan. Their VMA/driver origin is not distinguished; Rust's
/// global allocator cannot otherwise observe default native host allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum HostEvent {
    Allocate,
    Reallocate,
    Deallocate,
}

/// Observed native events with backend-specific units, rather than net memory
/// or a uniform count of native API calls. Object/resource factory attempts
/// include failures; destroy events mean the final Renderer owner release.
/// COM reference clones/releases are not independent object creation/destruction.
/// Vulkan memory allocation events are successful VMA device-memory callback
/// notifications. D3D12 committed-resource factories count their implicit heap
/// allocation attempts as well as resource attempts. Descriptor events count
/// requested/freed Vulkan sets or D3D12 heap descriptor elements, not calls.
/// These categories overlap and must not be summed as physical allocations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum NativeEvent {
    ObjectCreate,
    ObjectDestroy,
    ResourceCreate,
    ResourceDestroy,
    MemoryAllocate,
    MemoryFree,
    DescriptorAllocate,
    DescriptorFree,
    /// Reuse/reset itself is allowed. A reset that implicitly frees descriptor
    /// sets must additionally count those sets as [`Self::DescriptorFree`].
    Reset,
}

/// Cumulative process counters. Snapshot only at quiescent boundaries for exact
/// totals: independent atomic fields are not one transactional live snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    pub rust: [EventCount; 4],
    pub vma_host: [EventCount; 3],
    pub native: [u64; 9],
    pub overflowed: bool,
}

impl Snapshot {
    pub fn now() -> Self {
        COUNTERS.snapshot(HeapSnapshot::now())
    }

    /// Checked difference over one interval (or a contained event subtotal).
    /// Decreasing fields invalidate the observation rather than wrapping to a
    /// plausible zero. Do not reset shared counters while other threads can
    /// allocate: cumulative subtraction preserves delayed frees.
    pub fn since(self, earlier: Self) -> Self {
        fn subtract(current: u64, previous: u64, overflowed: &mut bool) -> u64 {
            current.checked_sub(previous).unwrap_or_else(|| {
                *overflowed = true;
                0
            })
        }
        let mut overflowed = self.overflowed || earlier.overflowed;
        let rust = std::array::from_fn(|i| self.rust[i].since(earlier.rust[i], &mut overflowed));
        let vma_host =
            std::array::from_fn(|i| self.vma_host[i].since(earlier.vma_host[i], &mut overflowed));
        let native =
            std::array::from_fn(|i| subtract(self.native[i], earlier.native[i], &mut overflowed));
        Self {
            rust,
            vma_host,
            native,
            overflowed,
        }
    }

    /// Accumulate disjoint intervals in fixed storage. Overflow saturates and
    /// invalidates zero acceptance; it cannot wrap a nonzero total back to zero.
    pub fn accumulate(&mut self, value: Self) {
        fn add(total: &mut u64, value: u64, overflowed: &mut bool) {
            *total = total.checked_add(value).unwrap_or_else(|| {
                *overflowed = true;
                u64::MAX
            });
        }
        self.overflowed |= value.overflowed;
        for (total, value) in self
            .rust
            .iter_mut()
            .zip(value.rust)
            .chain(self.vma_host.iter_mut().zip(value.vma_host))
        {
            total.accumulate(value, &mut self.overflowed);
        }
        for (total, value) in self.native.iter_mut().zip(value.native) {
            add(total, value, &mut self.overflowed);
        }
    }

    /// All observed allocation/free event counts are zero. Reset/reuse may
    /// occur; implicit frees must also be classified as descriptor free events.
    /// A zero snapshot cannot establish coverage of missing instrumentation.
    pub fn is_allocation_free(&self) -> bool {
        !self.overflowed
            && self.rust.iter().all(|event| event.calls == 0)
            && self.vma_host.iter().all(|event| event.calls == 0)
            && self.native[..NativeEvent::Reset as usize]
                .iter()
                .all(|count| *count == 0)
    }
}

struct Counters {
    host_calls: [AtomicU64; 3],
    host_bytes: [AtomicU64; 3],
    native: [AtomicU64; 9],
    overflowed: AtomicBool,
}

impl Counters {
    const fn new() -> Self {
        Self {
            host_calls: [const { AtomicU64::new(0) }; 3],
            host_bytes: [const { AtomicU64::new(0) }; 3],
            native: [const { AtomicU64::new(0) }; 9],
            overflowed: AtomicBool::new(false),
        }
    }

    fn add(&self, counter: &AtomicU64, amount: u64) {
        if counter.fetch_add(amount, Ordering::Relaxed) > u64::MAX - amount {
            self.overflowed.store(true, Ordering::Relaxed);
        }
    }

    fn host(&self, event: HostEvent, bytes: usize) {
        self.add(&self.host_calls[event as usize], 1);
        self.add(&self.host_bytes[event as usize], bytes as u64);
    }

    fn snapshot(&self, heap: HeapSnapshot) -> Snapshot {
        Snapshot {
            rust: heap.events,
            vma_host: std::array::from_fn(|i| EventCount {
                calls: self.host_calls[i].load(Ordering::Relaxed),
                bytes: self.host_bytes[i].load(Ordering::Relaxed),
            }),
            native: std::array::from_fn(|i| self.native[i].load(Ordering::Relaxed)),
            overflowed: heap.overflowed || self.overflowed.load(Ordering::Relaxed),
        }
    }
}

static COUNTERS: Counters = Counters::new();

/// Record the number of observed native events/units described by [`NativeEvent`].
/// This hook has no allocation, logging, failure handling, or required native
/// side effects. A missing event hook cannot be proved by a zero count.
pub fn record_native(event: NativeEvent, count: u64) {
    COUNTERS.add(&COUNTERS.native[event as usize], count);
}

/// Record a VMA host callback; forwarding the allocation/free is the callback's
/// separate required responsibility and must not depend on this feature.
pub fn record_vma_host(event: HostEvent, requested_bytes: usize) {
    COUNTERS.host(event, requested_bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_classes_overflow_and_zero_contract_are_independent() {
        let counters = Counters::new();
        let initial = counters.snapshot(HeapSnapshot::default());
        let mut heap = HeapSnapshot::default();
        heap.events[RustEvent::AllocateZeroed as usize] = EventCount {
            calls: 1,
            bytes: 32,
        };
        counters.host(HostEvent::Deallocate, 0);
        counters.add(&counters.native[NativeEvent::Reset as usize], 1);
        let delta = counters.snapshot(heap).since(initial);
        assert_eq!(
            delta.rust[RustEvent::AllocateZeroed as usize],
            EventCount {
                calls: 1,
                bytes: 32
            }
        );
        assert_eq!(delta.vma_host[HostEvent::Deallocate as usize].calls, 1);
        assert!(!delta.is_allocation_free());
        let mut sum = initial;
        sum.accumulate(delta);
        assert_eq!(sum, delta);
        sum.native[NativeEvent::ObjectCreate as usize] = u64::MAX;
        let mut extra = Snapshot::default();
        extra.native[NativeEvent::ObjectCreate as usize] = 1;
        sum.accumulate(extra);
        assert!(sum.overflowed);
        assert_eq!(sum.native[NativeEvent::ObjectCreate as usize], u64::MAX);
        let invalid = initial.since(delta);
        assert!(invalid.overflowed);
        assert!(!invalid.is_allocation_free());
        assert_eq!(invalid.rust[RustEvent::AllocateZeroed as usize].calls, 0);
        let reset_only = Snapshot {
            native: [0, 0, 0, 0, 0, 0, 0, 0, 1],
            ..Snapshot::default()
        };
        assert!(reset_only.is_allocation_free());
        counters.add(&counters.native[0], u64::MAX);
        counters.add(&counters.native[0], 1);
        assert!(counters.snapshot(HeapSnapshot::default()).overflowed);
    }
}

//! Optional cumulative allocation events, independent of validation.
//!
//! Install [`CountingAllocator`] around the application's global allocator to
//! observe Rust heap requests. Backend hooks separately observe Renderer-issued
//! native calls and VMA host metadata. Driver-internal allocations are not
//! observed. Snapshots and subtraction allocate nothing; format reports only
//! after the measured owners have been returned or dropped.
//! VMA callbacks should forward directly to a native allocator or `System`,
//! rather than the counting global allocator, to avoid counting the same host
//! allocation in both the Rust and VMA rows. Native event categories describe
//! separate requests, not one additive count of physical allocations.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Separate allocator entry points; an allocation made before the measurement
/// still contributes a deallocation when it is freed inside the interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum RustEvent {
    Allocate,
    AllocateZeroed,
    Reallocate,
    Deallocate,
}

/// Host allocation callbacks made by VMA, which Rust's global allocator cannot
/// observe when the native library uses its default C/C++ allocator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum HostEvent {
    Allocate,
    Reallocate,
    Deallocate,
}

/// Renderer-issued native requests. Count creation attempts before the call,
/// including failures, and destruction at the final Renderer owner release.
/// COM reference clones/releases are not independent object creation/destruction.
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

/// Event requests and their requested bytes, rather than resident/peak memory.
/// Free callbacks with no known size can report zero bytes while counting frees.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventCount {
    pub calls: u64,
    pub bytes: u64,
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
        COUNTERS.snapshot()
    }

    /// Difference over one interval. Do not reset shared counters while other
    /// threads can allocate: cumulative subtraction preserves delayed frees.
    pub fn since(self, earlier: Self) -> Self {
        let difference = |current: EventCount, previous: EventCount| EventCount {
            calls: current.calls.wrapping_sub(previous.calls),
            bytes: current.bytes.wrapping_sub(previous.bytes),
        };
        Self {
            rust: std::array::from_fn(|i| difference(self.rust[i], earlier.rust[i])),
            vma_host: std::array::from_fn(|i| difference(self.vma_host[i], earlier.vma_host[i])),
            native: std::array::from_fn(|i| self.native[i].wrapping_sub(earlier.native[i])),
            overflowed: self.overflowed || earlier.overflowed,
        }
    }

    /// All observed allocation/free request counts are zero. Reset/reuse may
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
    rust_calls: [AtomicU64; 4],
    rust_bytes: [AtomicU64; 4],
    host_calls: [AtomicU64; 3],
    host_bytes: [AtomicU64; 3],
    native: [AtomicU64; 9],
    overflowed: AtomicBool,
}

impl Counters {
    const fn new() -> Self {
        Self {
            rust_calls: [const { AtomicU64::new(0) }; 4],
            rust_bytes: [const { AtomicU64::new(0) }; 4],
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

    fn rust(&self, event: RustEvent, bytes: usize) {
        self.add(&self.rust_calls[event as usize], 1);
        self.add(&self.rust_bytes[event as usize], bytes as u64);
    }

    fn host(&self, event: HostEvent, bytes: usize) {
        self.add(&self.host_calls[event as usize], 1);
        self.add(&self.host_bytes[event as usize], bytes as u64);
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            rust: std::array::from_fn(|i| EventCount {
                calls: self.rust_calls[i].load(Ordering::Relaxed),
                bytes: self.rust_bytes[i].load(Ordering::Relaxed),
            }),
            vma_host: std::array::from_fn(|i| EventCount {
                calls: self.host_calls[i].load(Ordering::Relaxed),
                bytes: self.host_bytes[i].load(Ordering::Relaxed),
            }),
            native: std::array::from_fn(|i| self.native[i].load(Ordering::Relaxed)),
            overflowed: self.overflowed.load(Ordering::Relaxed),
        }
    }
}

static COUNTERS: Counters = Counters::new();

/// Record an actual Renderer native call. This hook has no allocation, logging,
/// failure handling, or required native side effects.
pub fn record_native(event: NativeEvent, count: u64) {
    COUNTERS.add(&COUNTERS.native[event as usize], count);
}

/// Record a VMA host callback; forwarding the allocation/free is the callback's
/// separate required responsibility and must not depend on this feature.
pub fn record_vma_host(event: HostEvent, requested_bytes: usize) {
    COUNTERS.host(event, requested_bytes);
}

/// A forwarding global allocator wrapper for a measurement executable.
/// The library never selects an application's global allocator automatically.
pub struct CountingAllocator<A> {
    inner: A,
}

impl<A> CountingAllocator<A> {
    pub const fn new(inner: A) -> Self {
        Self { inner }
    }
}

// SAFETY: Each method forwards the original pointer/Layout and operation to A;
// recording uses only fixed atomic storage and cannot allocate or unwind.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNTERS.rust(RustEvent::Allocate, layout.size());
        // SAFETY: Forward the caller's valid allocation contract unchanged.
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        COUNTERS.rust(RustEvent::AllocateZeroed, layout.size());
        // SAFETY: Forward the caller's valid zeroed allocation contract.
        unsafe { self.inner.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        COUNTERS.rust(RustEvent::Reallocate, new_size);
        // SAFETY: Forward the original allocator's live pointer and Layout.
        unsafe { self.inner.realloc(pointer, layout, new_size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        COUNTERS.rust(RustEvent::Deallocate, layout.size());
        // SAFETY: Forward exactly the original allocation's pointer and Layout.
        unsafe { self.inner.dealloc(pointer, layout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;

    #[test]
    fn event_classes_overflow_and_zero_contract_are_independent() {
        let counters = Counters::new();
        let initial = counters.snapshot();
        counters.rust(RustEvent::AllocateZeroed, 32);
        counters.host(HostEvent::Deallocate, 0);
        counters.add(&counters.native[NativeEvent::Reset as usize], 1);
        let delta = counters.snapshot().since(initial);
        assert_eq!(
            delta.rust[RustEvent::AllocateZeroed as usize],
            EventCount {
                calls: 1,
                bytes: 32
            }
        );
        assert_eq!(delta.vma_host[HostEvent::Deallocate as usize].calls, 1);
        assert!(!delta.is_allocation_free());
        let reset_only = Snapshot {
            native: [0, 0, 0, 0, 0, 0, 0, 0, 1],
            ..Snapshot::default()
        };
        assert!(reset_only.is_allocation_free());
        counters.add(&counters.native[0], u64::MAX);
        counters.add(&counters.native[0], 1);
        assert!(counters.snapshot().overflowed);
    }

    #[test]
    fn forwarding_allocator_observes_each_operation_and_a_delayed_free() {
        let allocator = CountingAllocator::new(System);
        let small = Layout::from_size_align(16, 16).unwrap();
        let large = Layout::from_size_align(32, 16).unwrap();
        // SAFETY: Both allocations have valid nonzero Layouts; each successful
        // pointer is resized/freed only using this same allocator and Layout.
        unsafe {
            let prior = allocator.alloc(small);
            assert!(!prior.is_null());
            let start = Snapshot::now();
            let zeroed = allocator.alloc_zeroed(small);
            assert!(!zeroed.is_null());
            assert_eq!(*zeroed, 0);
            let grown = allocator.realloc(zeroed, small, 32);
            assert!(!grown.is_null());
            allocator.dealloc(grown, large);
            allocator.dealloc(prior, small);
            let delta = Snapshot::now().since(start);
            // Other parallel tests may also use counters; these are lower
            // bounds, while local fixed-counter tests check exact indexing.
            assert!(delta.rust[RustEvent::AllocateZeroed as usize].calls >= 1);
            assert!(delta.rust[RustEvent::Reallocate as usize].calls >= 1);
            assert!(delta.rust[RustEvent::Deallocate as usize].calls >= 2);
            assert!(!delta.is_allocation_free());
        }
    }
}

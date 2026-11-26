//! Count Rust heap requests in executables that install [`CountingAllocator`].
//! Take snapshots at quiescent boundaries to observe exact totals across threads.

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

/// Event requests and their requested bytes, rather than resident/peak memory.
/// Free callbacks with no known size can report zero bytes while counting frees.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventCount {
    pub calls: u64,
    pub bytes: u64,
}

impl EventCount {
    /// Checked interval difference. Mark a decreasing count or byte total as overflowed.
    pub fn since(self, previous: Self, overflowed: &mut bool) -> Self {
        let subtract = |current: u64, previous: u64, overflowed: &mut bool| {
            current.checked_sub(previous).unwrap_or_else(|| {
                *overflowed = true;
                0
            })
        };
        Self {
            calls: subtract(self.calls, previous.calls, overflowed),
            bytes: subtract(self.bytes, previous.bytes, overflowed),
        }
    }

    /// Add a disjoint interval, saturating and invalidating the observation on overflow.
    pub fn accumulate(&mut self, value: Self, overflowed: &mut bool) {
        for (total, value) in [
            (&mut self.calls, value.calls),
            (&mut self.bytes, value.bytes),
        ] {
            *total = total.checked_add(value).unwrap_or_else(|| {
                *overflowed = true;
                u64::MAX
            });
        }
    }
}

/// Cumulative allocation, zeroed allocation, reallocation and deallocation totals.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeapSnapshot {
    pub events: [EventCount; 4],
    pub overflowed: bool,
}

impl HeapSnapshot {
    pub fn now() -> Self {
        Self {
            events: std::array::from_fn(|i| EventCount {
                calls: CALLS[i].load(Ordering::Relaxed),
                bytes: BYTES[i].load(Ordering::Relaxed),
            }),
            overflowed: OVERFLOWED.load(Ordering::Relaxed),
        }
    }

    pub fn since(self, earlier: Self) -> Self {
        let mut overflowed = self.overflowed || earlier.overflowed;
        let events =
            std::array::from_fn(|i| self.events[i].since(earlier.events[i], &mut overflowed));
        Self { events, overflowed }
    }

    pub fn accumulate(&mut self, interval: Self) {
        self.overflowed |= interval.overflowed;
        for (total, value) in self.events.iter_mut().zip(interval.events) {
            total.accumulate(value, &mut self.overflowed);
        }
    }

    pub fn is_allocation_free(&self) -> bool {
        !self.overflowed && self.events.iter().all(|event| event.calls == 0)
    }
}

static CALLS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static BYTES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static OVERFLOWED: AtomicBool = AtomicBool::new(false);

fn record(event: RustEvent, bytes: usize) {
    let index = event as usize;
    let previous_calls = CALLS[index].fetch_add(1, Ordering::Relaxed);
    let previous_bytes = BYTES[index].fetch_add(bytes as u64, Ordering::Relaxed);
    if previous_calls == u64::MAX || previous_bytes > u64::MAX - bytes as u64 {
        OVERFLOWED.store(true, Ordering::Relaxed);
    }
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
        record(RustEvent::Allocate, layout.size());
        // SAFETY: Forward the caller's valid allocation contract unchanged.
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(RustEvent::AllocateZeroed, layout.size());
        // SAFETY: Forward the caller's valid zeroed allocation contract.
        unsafe { self.inner.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(RustEvent::Reallocate, new_size);
        // SAFETY: Forward the original allocator's live pointer and Layout.
        unsafe { self.inner.realloc(pointer, layout, new_size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record(RustEvent::Deallocate, layout.size());
        // SAFETY: Forward exactly the original allocation's pointer and Layout.
        unsafe { self.inner.dealloc(pointer, layout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;

    #[test]
    fn wrapped_or_saturated_intervals_never_report_a_quiet_heap() {
        let mut earlier = HeapSnapshot::default();
        earlier.events[RustEvent::Allocate as usize].calls = u64::MAX;
        let wrapped = HeapSnapshot::default().since(earlier);
        assert!(wrapped.overflowed);
        assert!(!wrapped.is_allocation_free());

        let mut extra = HeapSnapshot::default();
        extra.events[RustEvent::Allocate as usize].calls = 1;
        earlier.accumulate(extra);
        assert!(earlier.overflowed);
        assert_eq!(earlier.events[RustEvent::Allocate as usize].calls, u64::MAX);
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
            let start = HeapSnapshot::now();
            let zeroed = allocator.alloc_zeroed(small);
            assert!(!zeroed.is_null());
            assert_eq!(*zeroed, 0);
            let grown = allocator.realloc(zeroed, small, 32);
            assert!(!grown.is_null());
            allocator.dealloc(grown, large);
            allocator.dealloc(prior, small);
            let delta = HeapSnapshot::now().since(start);
            // Only this explicitly wrapped allocator contributes to this process's counters.
            assert_eq!(delta.events[RustEvent::AllocateZeroed as usize].calls, 1);
            assert_eq!(delta.events[RustEvent::Reallocate as usize].calls, 1);
            assert_eq!(delta.events[RustEvent::Deallocate as usize].calls, 2);
            assert!(!delta.is_allocation_free());
        }
    }
}

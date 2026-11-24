//! Per-process live Rust allocation sizes in the measurement executable.
use renderer_core::measurement::CountingAllocator;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

pub struct Heap {
    inner: CountingAllocator<System>,
    live: AtomicUsize,
    peak: AtomicUsize,
}
impl Heap {
    pub const fn new() -> Self {
        Self {
            inner: CountingAllocator::new(System),
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }
    fn grow(&self, bytes: usize) {
        let before = self.live.fetch_add(bytes, Relaxed);
        self.peak.fetch_max(before + bytes, Relaxed);
    }
    pub fn start(&self) -> usize {
        let bytes = self.live.load(Relaxed);
        self.peak.store(bytes, Relaxed);
        bytes
    }
    pub fn peak(&self) -> usize {
        self.peak.load(Relaxed)
    }
}

// SAFETY: Each operation forwards the original layout and pointer to the same
// allocator. Atomic counters own no heap memory and cannot re-enter allocation.
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Forwarding the caller's valid allocation request.
        let p = unsafe { self.inner.alloc(layout) };
        if !p.is_null() {
            self.grow(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Forwarding the caller's valid zeroed allocation request.
        let p = unsafe { self.inner.alloc_zeroed(layout) };
        if !p.is_null() {
            self.grow(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        self.live.fetch_sub(layout.size(), Relaxed);
        // SAFETY: The caller supplies the original pointer and layout.
        unsafe { self.inner.dealloc(p, layout) };
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: The old allocation and the requested size satisfy GlobalAlloc.
        let new = unsafe { self.inner.realloc(p, layout, size) };
        if new.is_null() {
            return new;
        }
        if size >= layout.size() {
            self.grow(size - layout.size());
        } else {
            self.live.fetch_sub(layout.size() - size, Relaxed);
        }
        new
    }
}

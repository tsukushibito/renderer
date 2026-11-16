//! CPU-only full reuse cycles through the production Vulkan candidate index.
//! Build/setup and JSON formatting are outside the measured allocation window.
//! No Vulkan objects are created and no GPU speed claim follows from this test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[path = "../src/backends/vulkan/graph/cache.rs"]
#[allow(
    dead_code,
    unused_imports,
    reason = "CPU benchmark includes private code without its unit-test harness"
)]
mod cache;
use cache::{CacheIndex, CacheTarget};

struct CountingAllocator;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ZEROED_ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static REALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
// SAFETY: Every operation forwards the original layout/pointer to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller supplies GlobalAlloc's required valid layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ZEROED_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller supplies GlobalAlloc's required valid layout.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller supplies the live allocation and its original layout.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller supplies GlobalAlloc's valid pointer/layout/new size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn counters() -> [u64; 4] {
    [
        ALLOCATIONS.load(Ordering::Relaxed),
        ZEROED_ALLOCATIONS.load(Ordering::Relaxed),
        REALLOCATIONS.load(Ordering::Relaxed),
        DEALLOCATIONS.load(Ordering::Relaxed),
    ]
}

fn cycle(index: &mut CacheIndex<usize>, prefix: usize, count: usize, classes: usize) {
    let capacity = prefix + count;
    for class in 0..classes {
        for offset in (class..count).step_by(classes) {
            let target = index.select(black_box(class), capacity, capacity, None, |_| true);
            assert_eq!(target, Ok(CacheTarget::Reuse(prefix + offset)));
            index.remove(prefix + offset);
        }
    }
    // Reverse return order exercises candidate insertion independently of the
    // acquisition order instead of measuring only the easy append-only case.
    for offset in (0..count).rev() {
        index.insert(prefix + offset, offset % classes, 7);
    }
}

fn main() {
    let mut cases = Vec::new();
    for (name, prefix, count, classes, iterations) in [
        ("default64_single", 0, 64, 1, 512),
        ("default64_mixed8", 0, 64, 8, 512),
        ("default64_unique", 0, 64, 64, 512),
        ("large1024_single", 0, 1024, 1, 32),
        ("large1024_mixed16", 0, 1024, 16, 32),
        ("large1024_mixed512", 0, 1024, 512, 32),
        ("registered16384_transient1024", 16384, 1024, 16, 32),
        ("large8192_single", 0, 8192, 1, 1),
    ] {
        let mut index = CacheIndex::new(prefix + count).expect("prepare candidate index");
        for offset in 0..count {
            index.insert(prefix + offset, offset % classes, 7);
        }
        let warmup_started = Instant::now();
        let mut warmup_cycles = 0;
        while warmup_cycles < 8 || warmup_started.elapsed().as_millis() < 100 {
            cycle(&mut index, prefix, count, classes);
            warmup_cycles += 1;
        }
        let mut samples = [0.0; 31];
        let before = counters();
        for sample in &mut samples {
            let start = Instant::now();
            for _ in 0..iterations {
                cycle(black_box(&mut index), prefix, count, classes);
            }
            *sample = start.elapsed().as_nanos() as f64 / iterations as f64;
        }
        let after = counters();
        let events = std::array::from_fn::<_, 4, _>(|i| after[i] - before[i]);
        assert_eq!(
            events, [0; 4],
            "normal cycles must not allocate or release heap"
        );
        let mut sorted = samples;
        sorted.sort_by(f64::total_cmp);
        cases.push(format!(
            "{{\"name\":\"{name}\",\"registered\":{prefix},\"transients\":{count},\"classes\":{classes},\"warmup_cycles\":{warmup_cycles},\"iterations_per_sample\":{iterations},\"median_ns\":{},\"p95_ns\":{},\"heap_alloc_zeroed_realloc_dealloc\":{events:?},\"samples_ns\":{samples:?}}}",
            sorted[15], sorted[29],
        ));
    }
    println!(
        "{{\"schema\":1,\"sample_count\":31,\"minimum_warmup_cycles\":8,\"minimum_warmup_ms\":100,\"measurement\":\"acquire all then reverse-order return; CPU only\",\"cases\":[{}]}}",
        cases.join(",")
    );
}

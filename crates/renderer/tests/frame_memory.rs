//! Explicit measurement gates; use one test thread and an optimized build.
//! The standalone frame_memory example excludes libtest itself from the process.

#![cfg(feature = "memory-profiling")]

use renderer::measurement::CountingAllocator;
use std::alloc::System;

#[path = "../examples/support/frame_memory.rs"]
mod frame_memory;

#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);

#[test]
#[ignore = "explicit optimized full CPU lifecycle allocation gate; RENDERER_MEMORY_BASELINE=1 records a nonzero baseline"]
fn frame_memory_cpu_full_lifecycle() {
    let baseline = match std::env::var("RENDERER_MEMORY_BASELINE").as_deref() {
        Ok("1") => true,
        Err(std::env::VarError::NotPresent) => false,
        _ => panic!("RENDERER_MEMORY_BASELINE must be unset or 1"),
    };
    frame_memory::cpu_lifecycle(baseline);
}

//! Explicit measurement gates; use one test thread and an optimized build.
//! The standalone frame_memory example excludes libtest itself from the process.

#![cfg(feature = "memory-profiling")]

use renderer::measurement::CountingAllocator;
use std::alloc::System;

#[path = "../examples/support/image_transform.rs"]
mod image_transform;

#[path = "../examples/support/frame_memory.rs"]
mod frame_memory;

#[cfg(feature = "vulkan")]
#[path = "../examples/support/frame_memory_image.rs"]
mod frame_memory_image;

#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);

#[test]
#[ignore = "explicit optimized full CPU lifecycle allocation gate; RENDERER_MEMORY_BASELINE=1 records a nonzero baseline"]
fn frame_memory_cpu_full_lifecycle() {
    frame_memory::cpu_lifecycle(baseline());
}

fn baseline() -> bool {
    match std::env::var("RENDERER_MEMORY_BASELINE").as_deref() {
        Ok("1") => true,
        Err(std::env::VarError::NotPresent) => false,
        _ => panic!("RENDERER_MEMORY_BASELINE must be unset or 1"),
    }
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "explicit Vulkan public buffer full-lifecycle memory gate, including held readbacks; native hooks required"]
fn frame_memory_vulkan_buffer_full_lifecycle() {
    frame_memory::vulkan_buffer_lifecycle(baseline());
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "explicit optimized Vulkan Engine/Raster/Compute image full-lifecycle memory gate; native hooks required"]
fn frame_memory_vulkan_engine_image_full_lifecycle() {
    frame_memory_image::lifecycle(baseline());
}

#[cfg(all(feature = "d3d12", windows))]
#[test]
#[ignore = "explicit D3D12 public buffer full-lifecycle memory gate, including held readbacks; native hooks required"]
fn frame_memory_d3d12_buffer_full_lifecycle() {
    frame_memory::d3d12_buffer_lifecycle(baseline());
}

//! Standalone measurement, without libtest activity in the measurement process.

use renderer::measurement::CountingAllocator;
use std::alloc::System;

#[path = "support/frame_memory.rs"]
mod frame_memory;

#[cfg(feature = "vulkan")]
#[path = "support/frame_memory_image.rs"]
mod frame_memory_image;

#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "cpu".into());
    let baseline = match args.next().as_deref() {
        None => false,
        Some("--baseline") => true,
        Some(_) => panic!("expected optional --baseline"),
    };
    assert!(args.next().is_none(), "unexpected argument");
    match mode.as_str() {
        "cpu" => frame_memory::cpu_lifecycle(baseline),
        #[cfg(feature = "vulkan")]
        "vulkan" => frame_memory::vulkan_buffer_lifecycle(baseline),
        #[cfg(feature = "vulkan")]
        "vulkan-image" => frame_memory_image::lifecycle(baseline),
        #[cfg(all(feature = "d3d12", windows))]
        "d3d12" => frame_memory::d3d12_buffer_lifecycle(baseline),
        _ => panic!(
            "supported modes: cpu/vulkan/vulkan-image/d3d12 [--baseline] (backend feature required)"
        ),
    }
}

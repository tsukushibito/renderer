//! Standalone measurement, without libtest activity in the measurement process.

use renderer::measurement::CountingAllocator;
use std::alloc::System;

#[path = "support/frame_memory.rs"]
mod frame_memory;

#[cfg(feature = "vulkan")]
#[path = "support/frame_memory_image.rs"]
mod frame_memory_image;

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
#[path = "support/frame_memory_raster.rs"]
mod frame_memory_raster;

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
#[path = "support/compute_fixture.rs"]
mod compute_fixture;

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
#[path = "support/standard_fixture.rs"]
mod standard_fixture;

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
#[allow(dead_code)]
#[path = "../tests/support/custom_raster_fixture.rs"]
mod custom_raster_fixture;

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
#[path = "support/frame_memory_compute.rs"]
mod frame_memory_compute;

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
        #[cfg(feature = "vulkan")]
        "vulkan-raster" => frame_memory_raster::vulkan(baseline),
        #[cfg(feature = "vulkan")]
        "vulkan-compute" => frame_memory_compute::vulkan(baseline),
        #[cfg(feature = "vulkan")]
        "vulkan-standard" => frame_memory_compute::vulkan_standard(baseline),
        #[cfg(feature = "vulkan")]
        "vulkan-custom-raster" => frame_memory_compute::vulkan_custom_raster(baseline),
        #[cfg(all(feature = "d3d12", windows))]
        "d3d12" => frame_memory::d3d12_buffer_lifecycle(baseline),
        #[cfg(all(feature = "d3d12", windows))]
        "d3d12-raster" => frame_memory_raster::d3d12(baseline),
        #[cfg(all(feature = "d3d12", windows))]
        "d3d12-compute" => frame_memory_compute::d3d12(baseline),
        #[cfg(all(feature = "d3d12", windows))]
        "d3d12-standard" => frame_memory_compute::d3d12_standard(baseline),
        #[cfg(all(feature = "d3d12", windows))]
        "d3d12-custom-raster" => frame_memory_compute::d3d12_custom_raster(baseline),
        _ => panic!(
            "supported modes: cpu/vulkan/vulkan-image/vulkan-raster/vulkan-compute/vulkan-standard/vulkan-custom-raster/d3d12/d3d12-raster/d3d12-compute/d3d12-standard/d3d12-custom-raster [--baseline] (backend feature required)"
        ),
    }
}

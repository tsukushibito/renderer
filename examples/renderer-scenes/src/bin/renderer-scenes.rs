//! Bistro cook import, Rust render-graph composition, and bounded measurement.

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: sashiko_renderer_core::measurement::CountingAllocator<std::alloc::System> =
    sashiko_renderer_core::measurement::CountingAllocator::new(std::alloc::System);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    renderer_scenes::run_recipe()
}

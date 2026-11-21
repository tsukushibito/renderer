//! Bistro cook import, Rust render-graph composition, and bounded measurement.

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: renderer_core::measurement::CountingAllocator<std::alloc::System> =
    renderer_core::measurement::CountingAllocator::new(std::alloc::System);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    renderer_bistro::run_recipe()
}

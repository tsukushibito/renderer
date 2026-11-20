//! Consumer-owned Bistro cook import, PBR composition, and bounded measurement.

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: renderer::measurement::CountingAllocator<std::alloc::System> =
    renderer::measurement::CountingAllocator::new(std::alloc::System);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    renderer_bistro::run_manual()
}

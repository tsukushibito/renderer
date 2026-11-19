//! Consumer-owned Bistro cook import, PBR composition, and bounded measurement.

#[path = "support/bistro.rs"]
mod bistro;
#[path = "support/shader_binaries.rs"]
mod shader_binaries;

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: renderer::measurement::CountingAllocator<std::alloc::System> =
    renderer::measurement::CountingAllocator::new(std::alloc::System);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    bistro::main()
}

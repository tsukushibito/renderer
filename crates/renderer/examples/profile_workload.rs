//! Repeatable headless CPU sampling workload; use --help for its measurement contract.

#[path = "support/profile_workload.rs"]
mod profile_workload;

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: renderer::measurement::CountingAllocator<std::alloc::System> =
    renderer::measurement::CountingAllocator::new(std::alloc::System);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    profile_workload::main()
}

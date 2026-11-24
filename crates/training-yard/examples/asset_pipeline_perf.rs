//! Release measurement of catalog lookup, asset sharing and visible texture transitions.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "asset_pipeline_perf/run.rs"]
mod run;

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "asset_pipeline_perf/heap.rs"]
mod heap;
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[global_allocator]
static ALLOCATOR: heap::Heap = heap::Heap::new();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    return run::main();
    #[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
    Err("enable a native backend".into())
}

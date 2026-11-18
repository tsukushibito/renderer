//! Mixed-format MRT and ordered graphics Programs on Vulkan and D3D12.
//! Run with `--release --features vulkan,d3d12` on Windows.

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
#[path = "support/raster_batch_fixture.rs"]
mod fixture;

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    fixture::run()
}

#[cfg(not(all(windows, feature = "vulkan", feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("raster_batches requires Windows with the vulkan and d3d12 features".into())
}

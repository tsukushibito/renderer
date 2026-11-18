//! Encode typed indexed commands from a visibility mask and draw with both APIs.
//! Run with `--release --features vulkan,d3d12` on Windows.

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
#[path = "../fixtures/indexed_indirect_fixture.rs"]
mod fixture;

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    fixture::run()
}

#[cfg(not(all(windows, feature = "vulkan", feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("indexed_indirect requires Windows with the vulkan and d3d12 features".into())
}

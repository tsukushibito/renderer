//! Offscreen indexed draw scaling on Vulkan and D3D12.

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
#[path = "support/raster_draw_scaling.rs"]
mod scaling;

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    scaling::run()
}

#[cfg(not(all(windows, feature = "vulkan", feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("raster_draw_scaling requires Windows with the vulkan and d3d12 features".into())
}

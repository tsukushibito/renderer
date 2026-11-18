//! Compare 1× and 4× MSAA using the same Graph on Vulkan and D3D12.

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
#[path = "support/msaa.rs"]
mod msaa;

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
fn main() {
    msaa::run();
}

#[cfg(not(all(windows, feature = "vulkan", feature = "d3d12")))]
fn main() -> Result<(), &'static str> {
    Err("the MSAA comparison requires Windows and the vulkan,d3d12 features")
}

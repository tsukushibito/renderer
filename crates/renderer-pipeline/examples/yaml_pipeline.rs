//! Run the same prepared YAML assets on Vulkan and D3D12, with offscreen readback.

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
#[path = "support/pipeline_shaders.rs"]
mod pipeline_shaders;

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
#[path = "support/pipeline_workload.rs"]
mod pipeline_workload;

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    pipeline_workload::run()
}

#[cfg(not(all(windows, feature = "vulkan", feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("the dual-backend YAML example requires Windows and vulkan,d3d12 features".into())
}

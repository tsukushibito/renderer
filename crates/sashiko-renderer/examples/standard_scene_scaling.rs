//! Fixed-workload scaling of the public StandardPipeline frame loop.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "support/standard_scaling.rs"]
mod benchmark;

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    benchmark::run()
}

#[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn main() {
    eprintln!("Enable a native GPU backend for standard_scene_scaling.");
    std::process::exit(2);
}

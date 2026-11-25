//! Compare CPU direct draws and GPU fixed-slot indirect draws on the same packed mesh.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "support/packed_visibility_scaling.rs"]
mod benchmark;

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    benchmark::run()
}

#[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn main() {
    eprintln!("Enable a native backend to benchmark packed visibility.");
    std::process::exit(2)
}

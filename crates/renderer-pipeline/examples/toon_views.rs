//! Run the toon_views example with a GPU backend feature and pass vulkan or d3d12.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "support/toon_pipeline.rs"]
mod toon_pipeline;
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "support/views.rs"]
mod views;

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    views::run(views::Example::TwoViews, toon_pipeline::prepare)
}

#[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn main() {
    eprintln!("Enable the vulkan or d3d12 feature to run this example.");
    std::process::exit(2);
}

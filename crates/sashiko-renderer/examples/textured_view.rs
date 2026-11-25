//! Run the textured_view example with a GPU backend feature and pass vulkan or d3d12.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "support/views.rs"]
mod views;

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    views::run(views::Example::Textured, views::standard)
}

#[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn main() {
    eprintln!("Enable the vulkan or d3d12 feature to run this example.");
    std::process::exit(2);
}

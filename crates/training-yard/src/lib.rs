//! Candidate drawing interfaces exercised by the training-yard consumers.

#[cfg(feature = "graphics")]
pub mod canvas;

#[cfg(feature = "graphics")]
pub mod scene;

#[cfg(feature = "graphics")]
pub mod capture;

#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
pub mod window;

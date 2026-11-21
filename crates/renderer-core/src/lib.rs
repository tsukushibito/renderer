//! RenderGraph-centered renderer for game engine integration.
//!
//! The graph API builds and analyzes GPU-independent operation declarations.
//! The default configuration has no GPU, window-system, or native dependencies.
//! The `vulkan` feature adds a headless Vulkan 1.3 runtime; `d3d12` adds a Windows
//! hardware runtime. Native validation defaults to enabled with the independent
//! `development-validation` feature and disabled otherwise. The optional
//! `presentation` feature adds Windows window integration. Building a graph
//! never initializes a native backend; runtime creation is explicit.
//!
//! Engine-facing and graph APIs remain independent of Vulkan and VMA types.

#[cfg(test)]
extern crate self as renderer_core;

#[cfg(test)]
#[path = "../fixtures/image_transform.rs"]
mod image_transform;

pub mod capacity;
pub mod geometry;
pub mod graph;
mod image;
// Native unit tests exercise the public Unlit inputs against this test build of renderer.
#[cfg(test)]
#[path = "../../renderer/src/unlit_forward/input.rs"]
mod unlit_test_input;
pub use image::ImageFormat;
pub mod profiling;
pub use profiling::{GpuTiming, PresentationMetrics, SubmissionMetrics};
pub mod program;
pub mod runtime;

#[cfg(feature = "presentation")]
pub mod presentation;

#[cfg(feature = "memory-profiling")]
pub mod measurement;

// Existing ignored native acceptance tests run in this binary. Normal library
// consumers still choose their own allocator; this affects unit-test builds only.
#[cfg(all(test, feature = "memory-profiling"))]
#[global_allocator]
static TEST_ALLOCATOR: measurement::CountingAllocator<std::alloc::System> =
    measurement::CountingAllocator::new(std::alloc::System);

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
mod backends;

#[cfg(feature = "vulkan")]
pub mod vulkan;

#[cfg(all(feature = "d3d12", windows))]
pub mod d3d12;

#[cfg(feature = "presentation")]
pub use presentation::{PresentOutcome, PresentationConfig};

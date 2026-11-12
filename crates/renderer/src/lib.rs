//! RenderGraph-centered renderer for game engine integration.
//!
//! The graph API builds and analyzes GPU-independent operation declarations.
//! The default configuration has no GPU, window-system, or native dependencies.
//! The `vulkan` feature adds a validation-required, headless Vulkan 1.3 runtime.
//! Building a graph never initializes Vulkan; runtime creation is explicit.
//!
//! Engine-facing and graph APIs remain independent of Vulkan and VMA types.

pub mod graph;

#[cfg(feature = "vulkan")]
mod backends;

#[cfg(feature = "vulkan")]
pub use backends::vulkan::graph::{
    RuntimeConfig, RuntimeError, RuntimeErrorKind, RuntimeStats, Submission, SubmissionResult,
    VulkanRuntime,
};

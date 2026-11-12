//! RenderGraph-centered renderer for game engine integration.
//!
//! The graph API builds and analyzes GPU-independent operation declarations.
//! The default configuration has no GPU, window-system, or native dependencies.
//! The `vulkan` feature adds a headless Vulkan 1.3 runtime with validation on by
//! default. The optional `presentation` feature adds Windows window integration.
//! Building a graph never initializes Vulkan; runtime creation is explicit.
//!
//! Engine-facing and graph APIs remain independent of Vulkan and VMA types.

pub mod engine;
pub mod graph;

#[cfg(feature = "vulkan")]
mod backends;

#[cfg(feature = "vulkan")]
pub use backends::vulkan::engine::{
    EngineRenderError, PreparedEngineFrame, RenderedFrame, VulkanEngine,
};

#[cfg(feature = "vulkan")]
pub use backends::vulkan::graph::{
    DeviceInfo, FrameCompletion, FrameId, FrameSubmission, GpuTiming, MemoryStats, RuntimeConfig,
    RuntimeError, RuntimeErrorKind, RuntimeStats, StagingMemoryStats, Submission,
    SubmissionMetrics, SubmissionResult, ValidationMonitor, ValidationReport, VulkanRuntime,
};

#[cfg(feature = "vulkan")]
pub use backends::vulkan::ValidationMode;

#[cfg(all(feature = "presentation", windows))]
pub use backends::vulkan::graph::{PresentOutcome, PresentationConfig, Presenter};

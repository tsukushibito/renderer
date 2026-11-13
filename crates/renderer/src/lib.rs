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

pub mod engine;
pub mod graph;

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
mod backends;

#[cfg(feature = "vulkan")]
pub use backends::vulkan::engine::{
    EngineRenderError, MeshCacheConfig, MeshCacheStats, MeshPreparationError, MeshUploadProgress,
    MeshUploadState, PreparedEngineFrame, RenderedFrame, VulkanEngine,
};

#[cfg(feature = "vulkan")]
pub use backends::vulkan::graph::{
    DeviceInfo, FrameCompletion, FrameId, FrameSubmission, GpuTiming, MemoryStats, RuntimeConfig,
    RuntimeError, RuntimeErrorKind, RuntimeStats, StagingMemoryStats, Submission,
    SubmissionMetrics, SubmissionResult, ValidationMonitor, ValidationReport, VulkanRuntime,
};

#[cfg(feature = "vulkan")]
pub use backends::vulkan::ValidationMode;

#[cfg(all(feature = "d3d12", windows))]
pub use backends::d3d12::{
    D3D12DeviceInfo, D3D12Runtime, D3D12RuntimeConfig, D3D12RuntimeError, D3D12RuntimeErrorKind,
    D3D12ValidationMessage, D3D12ValidationMode, D3D12ValidationMonitor, D3D12ValidationReport,
};

#[cfg(all(feature = "presentation", windows))]
pub use backends::vulkan::graph::{PresentOutcome, PresentationConfig, Presenter};

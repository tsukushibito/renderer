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
extern crate self as renderer;

#[cfg(test)]
#[path = "../fixtures/image_transform.rs"]
mod image_transform;

pub mod capacity;
pub mod engine;
pub mod geometry;
pub mod graph;
mod image;
pub use image::ImageFormat;
pub mod profiling;
pub use profiling::{GpuTiming, PresentationMetrics, SubmissionMetrics};
pub mod program;
#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
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
pub use backends::vulkan::engine::{
    CompiledEngineFrame, CompiledEngineGraph, EngineFrameCapacity, EngineRenderError,
    MeshCacheConfig, MeshCacheStats, MeshPreparationError, MeshUploadProgress, MeshUploadState,
    PreparedEngineFrame, PreparedEngineGraph, RenderedFrame, VulkanEngine,
};

#[cfg(feature = "vulkan")]
pub use backends::vulkan::graph::{
    DeviceInfo, FrameCapacity, FrameCompletion, FrameId, FrameSubmission, MemoryHeapStats,
    MemoryStats, ResourceClass, RuntimeConfig, RuntimeError, RuntimeErrorKind, RuntimeStats,
    StagingMemoryStats, Submission, SubmissionResult, ValidationMonitor, ValidationReport,
    VulkanRuntime,
};

#[cfg(feature = "vulkan")]
pub use backends::vulkan::ValidationMode;

#[cfg(all(feature = "d3d12", windows))]
pub use backends::d3d12::{
    D3D12BufferClass, D3D12CommittedHeapStats, D3D12CommittedMemoryStats, D3D12DeviceInfo,
    D3D12FrameCapacity, D3D12ImageClass, D3D12MemoryStats, D3D12PreparedGraph, D3D12Runtime,
    D3D12RuntimeConfig, D3D12RuntimeError, D3D12RuntimeErrorKind, D3D12StagingMemoryStats,
    D3D12Submission, D3D12SubmissionResult, D3D12ValidationMessage, D3D12ValidationMode,
    D3D12ValidationMonitor, D3D12ValidationReport, D3D12VideoMemoryInfo,
};

#[cfg(all(feature = "d3d12", feature = "presentation", windows))]
pub use backends::d3d12::D3D12Presenter;
#[cfg(feature = "presentation")]
pub use presentation::{PresentOutcome, PresentationConfig};

#[cfg(all(feature = "presentation", feature = "vulkan", windows))]
pub use backends::vulkan::graph::Presenter;

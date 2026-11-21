//! Direct Vulkan device and submission APIs. [`VulkanEngine`] extends
//! [`VulkanRuntime`] with GPU residency and uploads for mesh snapshots.
//! Use [`crate::runtime`] to select the backend at runtime.

pub use crate::backends::vulkan::engine::{
    CompiledEngineFrame, CompiledEngineGraph, EngineFrameCapacity, EngineRenderError,
    MeshCacheConfig, MeshCacheStats, MeshPreparationError, MeshUploadProgress, MeshUploadState,
    PreparedEngineFrame, PreparedEngineGraph, RenderedFrame, VulkanEngine,
};

pub use crate::backends::vulkan::graph::{
    DeviceInfo, FrameCapacity, FrameCompletion, FrameId, FrameSubmission, MemoryHeapStats,
    MemoryStats, ResourceClass, RuntimeConfig, RuntimeError, RuntimeErrorKind, RuntimeStats,
    StagingMemoryStats, Submission, SubmissionResult, ValidationMonitor, ValidationReport,
    VulkanExternalDevice, VulkanImageState, VulkanRuntime,
};

pub use crate::backends::vulkan::ValidationMode;
pub use crate::backends::vulkan::{VulkanHostDevice, VulkanHostFeatures};

#[cfg(all(windows, feature = "presentation"))]
pub use crate::backends::vulkan::graph::Presenter;

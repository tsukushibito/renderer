//! Direct D3D12 device, submission and memory inspection APIs for Windows.
//! Use [`crate::runtime`] to select the backend at runtime.

pub use crate::backends::d3d12::{
    D3D12BufferClass, D3D12CommittedHeapStats, D3D12CommittedMemoryStats, D3D12DeviceInfo,
    D3D12FrameCapacity, D3D12ImageClass, D3D12MemoryStats, D3D12PreparedGraph, D3D12Runtime,
    D3D12RuntimeConfig, D3D12RuntimeError, D3D12RuntimeErrorKind, D3D12StagingMemoryStats,
    D3D12Submission, D3D12SubmissionResult, D3D12ValidationMessage, D3D12ValidationMode,
    D3D12ValidationMonitor, D3D12ValidationReport, D3D12VideoMemoryInfo,
};

#[cfg(all(feature = "d3d12", feature = "presentation", windows))]
pub use crate::backends::d3d12::D3D12Presenter;

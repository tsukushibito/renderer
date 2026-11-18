use crate::{
    graph::{BufferHandle, ImageHandle, ImportBuffer, ImportImage, PassHandle},
    profiling::SubmissionMetrics,
};

use super::{ExecutionError, FrameId};

/// Metadata for one accepted Graph. Dropping it releases the export metadata
/// while the runtime retains pending GPU work.
pub struct FrameSubmission(SubmissionInner);

enum SubmissionInner {
    #[cfg(feature = "vulkan")]
    Vulkan(crate::FrameSubmission),
    #[cfg(all(feature = "d3d12", windows))]
    D3D12(crate::backends::d3d12::QueuedFrameSubmission),
}

impl FrameSubmission {
    pub fn id(&self) -> FrameId {
        match &self.0 {
            #[cfg(feature = "vulkan")]
            SubmissionInner::Vulkan(submission) => submission.id(),
            #[cfg(all(feature = "d3d12", windows))]
            SubmissionInner::D3D12(submission) => submission.id,
        }
    }

    /// Snapshot of accepted scheduled contents. Use the registered key and
    /// Runtime::delete to release the registration when no longer needed.
    pub fn export_buffer(&self, handle: BufferHandle) -> Result<ImportBuffer, ExecutionError> {
        Ok(match &self.0 {
            #[cfg(feature = "vulkan")]
            SubmissionInner::Vulkan(submission) => submission.export_buffer(handle)?,
            #[cfg(all(feature = "d3d12", windows))]
            SubmissionInner::D3D12(submission) => submission.export_buffer(handle)?,
        })
    }

    /// Snapshot of accepted scheduled contents for follow-up Graphs or
    /// presentation on this runtime.
    pub fn export_image(&self, handle: ImageHandle) -> Result<ImportImage, ExecutionError> {
        Ok(match &self.0 {
            #[cfg(feature = "vulkan")]
            SubmissionInner::Vulkan(submission) => submission.export_image(handle)?,
            #[cfg(all(feature = "d3d12", windows))]
            SubmissionInner::D3D12(submission) => submission.export_image(handle)?,
        })
    }
}

#[cfg(feature = "vulkan")]
impl From<crate::FrameSubmission> for FrameSubmission {
    fn from(submission: crate::FrameSubmission) -> Self {
        Self(SubmissionInner::Vulkan(submission))
    }
}

#[cfg(all(feature = "d3d12", windows))]
impl From<crate::backends::d3d12::QueuedFrameSubmission> for FrameSubmission {
    fn from(submission: crate::backends::d3d12::QueuedFrameSubmission) -> Self {
        Self(SubmissionInner::D3D12(submission))
    }
}

/// An observed successful GPU completion. CPU readback storage remains leased
/// until consumed or dropped, independently of the GPU execution slot.
pub struct FrameCompletion(CompletionInner);

#[allow(clippy::large_enum_variant)] // Keep both native result leases inline.
enum CompletionInner {
    #[cfg(feature = "vulkan")]
    Vulkan(crate::FrameCompletion),
    #[cfg(all(feature = "d3d12", windows))]
    D3D12(crate::backends::d3d12::QueuedFrameCompletion),
}

impl FrameCompletion {
    pub fn id(&self) -> FrameId {
        match &self.0 {
            #[cfg(feature = "vulkan")]
            CompletionInner::Vulkan(completed) => completed.id(),
            #[cfg(all(feature = "d3d12", windows))]
            CompletionInner::D3D12(completed) => completed.id,
        }
    }

    pub fn metrics(&self) -> Option<&SubmissionMetrics> {
        match &self.0 {
            #[cfg(feature = "vulkan")]
            CompletionInner::Vulkan(completed) => completed.result.metrics(),
            #[cfg(all(feature = "d3d12", windows))]
            CompletionInner::D3D12(completed) => completed.result.metrics(),
        }
    }

    /// Exact byte length of the unconsumed readback.
    pub fn readback_len(&self, pass: PassHandle) -> Result<usize, ExecutionError> {
        Ok(match &self.0 {
            #[cfg(feature = "vulkan")]
            CompletionInner::Vulkan(completed) => completed.result.readback_len(pass)?,
            #[cfg(all(feature = "d3d12", windows))]
            CompletionInner::D3D12(completed) => completed.result.readback_len(pass)?,
        })
    }

    /// Copy an unconsumed readback into caller-owned storage of exactly the
    /// requested length. Failure preserves the readback lease.
    pub fn readback_into(
        &mut self,
        pass: PassHandle,
        output: &mut [u8],
    ) -> Result<(), ExecutionError> {
        match &mut self.0 {
            #[cfg(feature = "vulkan")]
            CompletionInner::Vulkan(completed) => completed.result.readback_into(pass, output)?,
            #[cfg(all(feature = "d3d12", windows))]
            CompletionInner::D3D12(completed) => completed.result.readback_into(pass, output)?,
        }
        Ok(())
    }
}

#[cfg(feature = "vulkan")]
impl From<crate::FrameCompletion> for FrameCompletion {
    fn from(completed: crate::FrameCompletion) -> Self {
        Self(CompletionInner::Vulkan(completed))
    }
}

#[cfg(all(feature = "d3d12", windows))]
impl From<crate::backends::d3d12::QueuedFrameCompletion> for FrameCompletion {
    fn from(completed: crate::backends::d3d12::QueuedFrameCompletion) -> Self {
        Self(CompletionInner::D3D12(completed))
    }
}

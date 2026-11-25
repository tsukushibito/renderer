use crate::{
    graph::{BufferHandle, ImageHandle, ImportBuffer, ImportImage, PassHandle},
    profiling::SubmissionMetrics,
};

use super::{ExecutionError, FrameId};

/// Metadata for one accepted Graph. Dropping it releases the export metadata
/// while the runtime retains pending GPU work.
pub struct FrameSubmission {
    inner: SubmissionInner,
    pub(super) frame_image: Option<super::ManagedImage>,
}

enum SubmissionInner {
    #[cfg(feature = "vulkan")]
    Vulkan(crate::vulkan::FrameSubmission),
    #[cfg(all(feature = "d3d12", windows))]
    D3D12(crate::backends::d3d12::QueuedFrameSubmission),
}

#[cfg_attr(
    not(any(feature = "vulkan", all(feature = "d3d12", windows))),
    expect(
        unused_variables,
        reason = "without a backend, the dispatcher has no runtime variants"
    )
)]
impl FrameSubmission {
    /// Take the image selected by Graph::export_frame_image. Dropping an untaken
    /// image also schedules release of a frame-created registration.
    pub fn take_frame_image(&mut self) -> Option<super::ManagedImage> {
        self.frame_image.take()
    }

    pub fn id(&self) -> FrameId {
        match self.inner {
            #[cfg(feature = "vulkan")]
            SubmissionInner::Vulkan(ref submission) => submission.id(),
            #[cfg(all(feature = "d3d12", windows))]
            SubmissionInner::D3D12(ref submission) => submission.id,
        }
    }

    /// Identity of the Graph whose work was accepted.
    pub fn graph_id(&self) -> u64 {
        match self.inner {
            #[cfg(feature = "vulkan")]
            SubmissionInner::Vulkan(ref submission) => submission.graph_id(),
            #[cfg(all(feature = "d3d12", windows))]
            SubmissionInner::D3D12(ref submission) => submission.graph_id(),
        }
    }

    /// Snapshot of accepted scheduled contents. Use the registered key and
    /// Runtime::delete to release the registration when no longer needed.
    pub fn export_buffer(&self, handle: BufferHandle) -> Result<ImportBuffer, ExecutionError> {
        match self.inner {
            #[cfg(feature = "vulkan")]
            SubmissionInner::Vulkan(ref submission) => {
                submission.export_buffer(handle).map_err(Into::into)
            }
            #[cfg(all(feature = "d3d12", windows))]
            SubmissionInner::D3D12(ref submission) => {
                submission.export_buffer(handle).map_err(Into::into)
            }
        }
    }

    /// Snapshot of accepted scheduled contents for follow-up Graphs or
    /// presentation on this runtime.
    pub fn export_image(&self, handle: ImageHandle) -> Result<ImportImage, ExecutionError> {
        match self.inner {
            #[cfg(feature = "vulkan")]
            SubmissionInner::Vulkan(ref submission) => {
                submission.export_image(handle).map_err(Into::into)
            }
            #[cfg(all(feature = "d3d12", windows))]
            SubmissionInner::D3D12(ref submission) => {
                submission.export_image(handle).map_err(Into::into)
            }
        }
    }
}

#[cfg(feature = "vulkan")]
impl From<crate::vulkan::FrameSubmission> for FrameSubmission {
    fn from(submission: crate::vulkan::FrameSubmission) -> Self {
        Self {
            inner: SubmissionInner::Vulkan(submission),
            frame_image: None,
        }
    }
}

#[cfg(all(feature = "d3d12", windows))]
impl From<crate::backends::d3d12::QueuedFrameSubmission> for FrameSubmission {
    fn from(submission: crate::backends::d3d12::QueuedFrameSubmission) -> Self {
        Self {
            inner: SubmissionInner::D3D12(submission),
            frame_image: None,
        }
    }
}

/// An observed successful GPU completion. CPU readback storage remains leased
/// until consumed or dropped, independently of the GPU execution slot.
pub struct FrameCompletion(CompletionInner);

#[allow(clippy::large_enum_variant)] // Keep both native result leases inline.
enum CompletionInner {
    #[cfg(feature = "vulkan")]
    Vulkan(crate::vulkan::FrameCompletion),
    #[cfg(all(feature = "d3d12", windows))]
    D3D12(crate::backends::d3d12::QueuedFrameCompletion),
}

#[cfg_attr(
    not(any(feature = "vulkan", all(feature = "d3d12", windows))),
    expect(
        unused_variables,
        reason = "without a backend, the dispatcher has no runtime variants"
    )
)]
impl FrameCompletion {
    pub fn id(&self) -> FrameId {
        match self.0 {
            #[cfg(feature = "vulkan")]
            CompletionInner::Vulkan(ref completed) => completed.id(),
            #[cfg(all(feature = "d3d12", windows))]
            CompletionInner::D3D12(ref completed) => completed.id,
        }
    }

    pub fn metrics(&self) -> Option<&SubmissionMetrics> {
        match self.0 {
            #[cfg(feature = "vulkan")]
            CompletionInner::Vulkan(ref completed) => completed.result.metrics(),
            #[cfg(all(feature = "d3d12", windows))]
            CompletionInner::D3D12(ref completed) => completed.result.metrics(),
        }
    }

    /// Exact byte length of the unconsumed readback.
    pub fn readback_len(&self, pass: PassHandle) -> Result<usize, ExecutionError> {
        match self.0 {
            #[cfg(feature = "vulkan")]
            CompletionInner::Vulkan(ref completed) => {
                completed.result.readback_len(pass).map_err(Into::into)
            }
            #[cfg(all(feature = "d3d12", windows))]
            CompletionInner::D3D12(ref completed) => {
                completed.result.readback_len(pass).map_err(Into::into)
            }
        }
    }

    /// Copy an unconsumed readback into caller-owned storage of exactly the
    /// requested length. Failure preserves the readback lease.
    pub fn readback_into(
        &mut self,
        pass: PassHandle,
        output: &mut [u8],
    ) -> Result<(), ExecutionError> {
        match self.0 {
            #[cfg(feature = "vulkan")]
            CompletionInner::Vulkan(ref mut completed) => completed
                .result
                .readback_into(pass, output)
                .map_err(Into::into),
            #[cfg(all(feature = "d3d12", windows))]
            CompletionInner::D3D12(ref mut completed) => completed
                .result
                .readback_into(pass, output)
                .map_err(Into::into),
        }
    }
}

#[cfg(feature = "vulkan")]
impl From<crate::vulkan::FrameCompletion> for FrameCompletion {
    fn from(completed: crate::vulkan::FrameCompletion) -> Self {
        Self(CompletionInner::Vulkan(completed))
    }
}

#[cfg(all(feature = "d3d12", windows))]
impl From<crate::backends::d3d12::QueuedFrameCompletion> for FrameCompletion {
    fn from(completed: crate::backends::d3d12::QueuedFrameCompletion) -> Self {
        Self(CompletionInner::D3D12(completed))
    }
}

//! Bounded runtime ownership of in-flight frames, separate from CPU results.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::time::Instant;

use super::exports::ExportLease;
use super::{
    BufferHandle, CompiledGraph, Duration, Export, GraphWork, ImageHandle, ImportBuffer,
    ImportImage, PendingTransfer, RuntimeError, Submission, SubmissionResult, VulkanRuntime,
};

pub use crate::runtime::FrameId;
pub(super) use crate::runtime::FrameQueue;
use crate::runtime::Observation;

/// Accepted frame metadata. Dropping this value does not wait or cancel work:
/// the runtime owns that work until explicit completion observation or shutdown.
/// Export snapshots refer to scheduled contents and are not CPU-readiness tokens.
#[derive(Clone, Debug)]
pub struct FrameSubmission {
    id: FrameId,
    graph: u64,
    exports: ExportLease,
}

impl FrameSubmission {
    pub fn id(&self) -> FrameId {
        self.id
    }

    pub fn export_buffer(&self, handle: BufferHandle) -> Result<ImportBuffer, RuntimeError> {
        if handle.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph export handle"));
        }
        self.exports
            .get(handle.index)
            .and_then(|value| match value {
                Export::Buffer(value) => Some(value),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::invalid("buffer was not exported"))
    }

    pub fn export_image(&self, handle: ImageHandle) -> Result<ImportImage, RuntimeError> {
        if handle.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph export handle"));
        }
        self.exports
            .get(handle.index)
            .and_then(|value| match value {
                Export::Image(value) => Some(value),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::invalid("image was not exported"))
    }
}

/// One observed successful frame. Native execution and upload resources have
/// already retired; unread CPU readback leases remain owned by `result`.
pub struct FrameCompletion {
    id: FrameId,
    pub result: SubmissionResult,
}

impl FrameCompletion {
    pub fn id(&self) -> FrameId {
        self.id
    }
}

pub(super) struct PendingFrame {
    id: FrameId,
    graph: u64,
    pending: PendingTransfer<GraphWork>,
}

impl PendingFrame {
    fn observe(&mut self, timeout: Duration) -> Observation<FrameCompletion, RuntimeError> {
        let waited = if timeout.is_zero() {
            self.pending.poll()
        } else {
            self.pending.wait(timeout)
        };
        match waited {
            Ok(None) => Observation::Pending,
            Ok(Some(work)) => match SubmissionResult::completed(self.graph, work) {
                Ok(result) => Observation::Complete(FrameCompletion {
                    id: self.id,
                    result,
                }),
                Err(error) => Observation::Failed {
                    error,
                    retired: true,
                },
            },
            Err(error) => Observation::Failed {
                error: RuntimeError::from(error).context(format_args!("frame {:?}", self.id)),
                retired: !self.pending.is_pending(),
            },
        }
    }
}

impl VulkanRuntime {
    /// Submit a frame without transferring pending GPU ownership to the caller.
    /// Exhaustion is reported before submission; completed work is reclaimed by
    /// `poll_frame`, `wait_frame` or `drain_frames`, never by dropping metadata.
    pub fn submit_frame(&mut self, graph: &CompiledGraph) -> Result<FrameSubmission, RuntimeError> {
        self.ensure_frame_capacity()?;
        let submission = self.submit(graph)?;
        Ok(self.enqueue_frame(submission))
    }

    pub(crate) fn set_frame_limit(&mut self, limit: usize) -> Result<(), RuntimeError> {
        if limit > self.config.execution_slots || !self.frames.set_limit(limit) {
            return Err(RuntimeError::invalid(
                "invalid or busy Graph frame queue limit",
            ));
        }
        Ok(())
    }

    pub(super) fn ensure_frame_capacity(&self) -> Result<(), RuntimeError> {
        if self.frames.is_full() {
            Err(RuntimeError::capacity_with(
                "in-flight frame capacity exhausted; poll or explicitly wait before retrying",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::ExecutionSlots,
                    CapacityRecovery::Poll,
                    Some(self.frames.limit() as u64),
                    None,
                ),
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn enqueue_frame(&mut self, submission: Submission) -> FrameSubmission {
        let Submission {
            id,
            graph,
            pending,
            exports,
        } = submission;
        debug_assert_eq!(id.owner, self.id);
        self.frames.push(PendingFrame { id, graph, pending });
        FrameSubmission { id, graph, exports }
    }

    /// Number of runtime-owned frames not yet removed by completion observation.
    /// A frame whose Fence has signaled still counts until observed.
    pub fn pending_frames(&self) -> usize {
        self.frames.len()
    }

    /// Observe one completed frame without waiting for an unfinished Fence.
    /// A pending or unproven failed operation remains owned by this runtime.
    /// Completed frames are returned individually so a later probe error cannot
    /// discard results already handed to the caller.
    pub fn poll_frame(&mut self) -> Result<Option<FrameCompletion>, RuntimeError> {
        self.wait_frame(Duration::ZERO)
    }

    /// Observe one completed frame, explicitly waiting within `timeout` if no
    /// result is already available. Returns `None` when empty or on timeout.
    /// Unlike `drain_frames`, this returns the result and its unread readbacks.
    /// Other pending frames and unproven failed work remain runtime-owned.
    ///
    /// All frames are first polled without blocking. If none has completed, only
    /// the oldest pending frame is waited with the remaining shared budget.
    /// The budget is subject to Vulkan timeout precision, not a strict wall-clock
    /// deadline. No queue/device-wide idle operation is used.
    pub fn wait_frame(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<FrameCompletion>, RuntimeError> {
        // Do not reject a stopped/lost Device before observing each actual Fence
        // result. This preserves the transfer retirement contract after loss.
        let result = self
            .frames
            .wait_one(timeout, |frame, budget| frame.observe(budget));
        self.collect_recycled();
        result
    }

    pub(crate) fn wait_for_frame(
        &mut self,
        id: FrameId,
        timeout: Duration,
    ) -> Result<Option<FrameCompletion>, RuntimeError> {
        if id.owner != self.id {
            return Err(RuntimeError::wrong_owner(
                "frame belongs to another runtime",
            ));
        }
        let result = self.frames.wait_for(
            |frame| frame.id == id,
            |frame| frame.observe(timeout),
            || RuntimeError::invalid("frame is no longer pending"),
        );
        self.collect_recycled();
        result
    }

    /// Explicit shutdown/backpressure wait, discarding completed frame results.
    /// Returns false on budget expiry and preserves all unproven pending work.
    /// The budget is shared across frames, subject to Vulkan timeout precision;
    /// it is not a strict wall-clock deadline. Read back desired results through
    /// `poll_frame` or `wait_frame` before this result-discarding operation.
    pub fn drain_frames(&mut self, timeout: Duration) -> Result<bool, RuntimeError> {
        let start = Instant::now();
        while self.pending_frames() != 0 {
            let completed = self.wait_frame(timeout.saturating_sub(start.elapsed()))?;
            if completed.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeErrorKind;

    #[test]
    fn exported_snapshots_are_indexed_and_still_check_graph_and_resource_kind() {
        use crate::graph::{BufferDesc, ImageDesc, ResourceKey};

        let graph = 17;
        let mut pool = super::super::exports::ExportPool::new(4_096, 1).unwrap();
        let mut exports = pool.acquire().unwrap();
        let initialized = crate::graph::ImportRanges::test_capture(std::slice::from_ref(&(0..4)));
        for index in (0..4_096).rev() {
            let key = ResourceKey {
                owner: 9,
                index: index as u32,
                generation: 1,
            };
            let value = if index % 2 == 0 {
                Export::Buffer(
                    ImportBuffer::new(key, 73, BufferDesc { size: 4 }, initialized.clone())
                        .unwrap(),
                )
            } else {
                Export::Image(ImportImage::test_initialized(
                    key,
                    73,
                    ImageDesc::rgba8(1, 1),
                ))
            };
            exports.insert(index, value).unwrap();
        }
        let frame = FrameSubmission {
            id: FrameId::new(9, 73),
            graph,
            exports,
        };
        for index in (0..4_096).rev() {
            let key = if index % 2 == 0 {
                frame
                    .export_buffer(BufferHandle { graph, index })
                    .unwrap()
                    .key()
            } else {
                frame
                    .export_image(ImageHandle { graph, index })
                    .unwrap()
                    .key()
            };
            assert_eq!(key.index as usize, index);
        }
        assert_eq!(
            frame
                .export_buffer(BufferHandle {
                    graph: 18,
                    index: 0
                })
                .unwrap_err()
                .kind(),
            RuntimeErrorKind::WrongOwner
        );
        assert_eq!(
            frame
                .export_buffer(BufferHandle { graph, index: 1 })
                .unwrap_err()
                .kind(),
            RuntimeErrorKind::InvalidInput
        );
        assert_eq!(
            frame
                .export_image(ImageHandle {
                    graph,
                    index: 4_096
                })
                .unwrap_err()
                .kind(),
            RuntimeErrorKind::InvalidInput
        );
    }
}

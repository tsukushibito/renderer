//! Bounded runtime ownership of in-flight frames, separate from CPU results.

use std::collections::BTreeMap;
use std::time::Instant;

use super::{
    BufferHandle, CompiledGraph, Duration, Export, GraphWork, ImageHandle, ImportBuffer,
    ImportImage, PendingTransfer, RuntimeError, RuntimeErrorKind, Submission, SubmissionResult,
    VulkanRuntime,
};

/// Identity of one accepted submission, scoped to its originating runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct FrameId {
    owner: u64,
    serial: u64,
}

impl FrameId {
    pub(super) fn new(owner: u64, serial: u64) -> Self {
        Self { owner, serial }
    }
}

/// Accepted frame metadata. Dropping this value does not wait or cancel work:
/// the runtime owns that work until explicit completion observation or shutdown.
/// Export snapshots refer to scheduled contents and are not CPU-readiness tokens.
#[derive(Clone, Debug)]
pub struct FrameSubmission {
    id: FrameId,
    graph: u64,
    exports: BTreeMap<usize, Export>,
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
            .get(&handle.index)
            .and_then(|value| match value {
                Export::Buffer(value) => Some(value.clone()),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::invalid("buffer was not exported"))
    }

    pub fn export_image(&self, handle: ImageHandle) -> Result<ImportImage, RuntimeError> {
        if handle.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph export handle"));
        }
        self.exports
            .get(&handle.index)
            .and_then(|value| match value {
                Export::Image(value) => Some(value.clone()),
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
                error: RuntimeError::from(error).context(format!("frame {:?}", self.id)),
                retired: !self.pending.is_pending(),
            },
        }
    }
}

enum Observation<R, E> {
    Pending,
    Complete(R),
    Failed { error: E, retired: bool },
}

/// The probe is the only external completion boundary. Keeping this collection
/// independent of a driver lets ownership/error transitions be tested on CPU.
pub(super) struct FrameQueue<T> {
    entries: Vec<T>,
    limit: usize,
}

impl<T> FrameQueue<T> {
    pub(super) fn new(limit: usize) -> Result<Self, RuntimeError> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(limit).map_err(|error| {
            RuntimeError::with_source(RuntimeErrorKind::Native, error)
                .context("reserving bounded frame bookkeeping")
        })?;
        Ok(Self { entries, limit })
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn is_full(&self) -> bool {
        self.len() == self.limit
    }

    fn push(&mut self, value: T) {
        // Capacity is checked before accepting GPU work, and the pre-reserved
        // collection is uniquely borrowed for the whole submit transaction.
        assert!(!self.is_full(), "frame capacity reserved before submit");
        self.entries.push(value);
    }

    fn observe<R, E>(
        &mut self,
        mut probe: impl FnMut(&mut T) -> Observation<R, E>,
    ) -> Result<Option<R>, E> {
        for index in 0..self.entries.len() {
            match probe(&mut self.entries[index]) {
                Observation::Pending => {}
                Observation::Complete(result) => {
                    self.entries.remove(index);
                    return Ok(Some(result));
                }
                Observation::Failed { error, retired } => {
                    if retired {
                        self.entries.remove(index);
                    }
                    return Err(error);
                }
            }
        }
        Ok(None)
    }
}

impl VulkanRuntime {
    /// Submit a frame without transferring pending GPU ownership to the caller.
    /// Exhaustion is reported before submission; completed work is reclaimed by
    /// `poll_frame` or `drain_frames`, never by dropping the returned metadata.
    pub fn submit_frame(&mut self, graph: &CompiledGraph) -> Result<FrameSubmission, RuntimeError> {
        self.ensure_frame_capacity()?;
        let submission = self.submit(graph)?;
        Ok(self.enqueue_frame(submission))
    }

    pub(super) fn ensure_frame_capacity(&self) -> Result<(), RuntimeError> {
        if self.frames.is_full() {
            Err(RuntimeError::capacity(
                "in-flight frame capacity exhausted; poll or explicitly wait before retrying",
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
        // Do not reject a stopped/lost Device before observing each actual Fence
        // result. This preserves the transfer retirement contract after loss.
        self.frames.observe(|frame| frame.observe(Duration::ZERO))
    }

    /// Explicit shutdown/backpressure wait, discarding completed frame results.
    /// Returns false on budget expiry and preserves all unproven pending work.
    /// The budget is shared across frames, subject to Vulkan timeout precision;
    /// it is not a strict wall-clock deadline. Read back desired results through
    /// `poll_frame` before using this result-discarding operation.
    pub fn drain_frames(&mut self, timeout: Duration) -> Result<bool, RuntimeError> {
        let start = Instant::now();
        while self.pending_frames() != 0 {
            let completed = self
                .frames
                .observe(|frame| frame.observe(timeout.saturating_sub(start.elapsed())))?;
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

    #[test]
    fn exported_snapshots_are_indexed_and_still_check_graph_and_resource_kind() {
        use crate::graph::{BufferDesc, ImageDesc, ResourceKey};

        let graph = 17;
        let exports = (0..4_096)
            .rev()
            .map(|index| {
                let key = ResourceKey {
                    owner: 9,
                    index: index as u32,
                    generation: 1,
                };
                let value = if index % 2 == 0 {
                    Export::Buffer(ImportBuffer {
                        key,
                        revision: 73,
                        desc: BufferDesc { size: 4 },
                        initialized: std::iter::once(0..4).collect(),
                    })
                } else {
                    Export::Image(ImportImage {
                        key,
                        revision: 73,
                        desc: ImageDesc::rgba8(1, 1),
                        initialized: true,
                    })
                };
                (index, value)
            })
            .collect();
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
    use std::cell::Cell;
    use std::rc::Rc;

    struct Work(Rc<Cell<usize>>);

    impl Drop for Work {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn bounded_queue_retains_unresolved_work_and_releases_only_retired_entries() {
        let drops = Rc::new(Cell::new(0));
        let mut frames = FrameQueue::new(2).unwrap();
        frames.push(Work(Rc::clone(&drops)));
        frames.push(Work(Rc::clone(&drops)));
        assert!(frames.is_full());
        assert_eq!(
            frames.observe::<(), &str>(|_| Observation::Pending),
            Ok(None)
        );
        assert_eq!(drops.get(), 0);
        assert_eq!(
            frames.observe::<(), _>(|_| Observation::Failed {
                error: "completion unproven",
                retired: false,
            }),
            Err("completion unproven")
        );
        assert_eq!(frames.len(), 2);
        assert_eq!(drops.get(), 0);
        assert_eq!(
            frames.observe::<_, &str>(|_| Observation::Complete(17)),
            Ok(Some(17))
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(drops.get(), 1);
        frames.push(Work(Rc::clone(&drops)));
        assert_eq!(
            frames.observe::<(), _>(|_| Observation::Failed {
                error: "device lost; no valid output",
                retired: true,
            }),
            Err("device lost; no valid output")
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(drops.get(), 2);
        drop(frames);
        assert_eq!(drops.get(), 3);
    }

    #[test]
    fn pending_front_does_not_hide_an_independently_observed_completion() {
        let mut frames = FrameQueue::new(2).unwrap();
        frames.push(false);
        frames.push(true);
        assert_eq!(
            frames.observe::<_, ()>(|ready| if *ready {
                Observation::Complete(23)
            } else {
                Observation::Pending
            }),
            Ok(Some(23))
        );
        assert_eq!(frames.entries, [false]);
    }
}

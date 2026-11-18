//! Lowers compiled Graphs into prepared work on one DIRECT queue.
//!
//! The shared compiled graph owns logical validity and scheduling. Preparation
//! checks backend support and capacity, leases prepared native operands, and
//! re-records changing work. Submission does not revalidate Graph contents.

use std::{mem::ManuallyDrop, sync::Arc, time::Duration};

use crate::graph::{CompiledGraph, PassHandle};

mod bounded;
mod raster;
mod transition_batch;

pub(super) use transition_batch::{TransitionBatch, common_capacity};

use super::{
    D3D12Runtime, D3D12RuntimeError,
    bindings::*,
    frame::{FrameResult, FrameSubmission, FrameWork},
};

pub(super) fn transition_to_common(
    slot: &mut super::frame::ExecutionSlot,
    list: &ID3D12GraphicsCommandList,
) -> Result<(), D3D12RuntimeError> {
    let required = common_capacity(
        slot.buffers_used,
        slot.raster
            .as_ref()
            .map_or(0, |storage| storage.images.len()),
    )?;
    slot.common_barriers.record(list, required, |batch| {
        for index in 0..slot.buffers_used {
            batch.push_transition(
                slot.buffers[index].raw(),
                slot.states[index],
                D3D12_RESOURCE_STATE_COMMON,
            )?;
        }
        batch.push_transition(
            slot.scratch.raw(),
            slot.scratch_state,
            D3D12_RESOURCE_STATE_COMMON,
        )?;
        if let Some(storage) = &slot.raster {
            for image in &storage.images {
                if image.used {
                    batch.push_transition(
                        image.native.raw(),
                        image.state,
                        D3D12_RESOURCE_STATE_COMMON,
                    )?;
                }
            }
        }
        Ok(())
    })?;
    // Publish planned states only after the whole batch was recorded. A
    // capacity/fill failure cancels unsubmitted work without changing these
    // states; canonical contents still commit only after successful Signal.
    slot.states[..slot.buffers_used].fill(D3D12_RESOURCE_STATE_COMMON);
    slot.scratch_state = D3D12_RESOURCE_STATE_COMMON;
    if let Some(storage) = &mut slot.raster {
        for image in &mut storage.images {
            if image.used {
                image.state = D3D12_RESOURCE_STATE_COMMON;
            }
        }
    }
    Ok(())
}

pub(super) fn transition(
    list: &ID3D12GraphicsCommandList,
    resource: &ID3D12Resource,
    before: &mut D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) {
    if *before == after {
        return;
    }
    let mut barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: *before,
                StateAfter: after,
            }),
        },
    };
    // SAFETY: The tagged union contains Transition, this private buffer is live,
    // and before records its last state within this single command list.
    unsafe {
        list.ResourceBarrier(std::slice::from_ref(&barrier));
        // The generated union deliberately suppresses field Drop. Balance the
        // temporary cloned resource reference after the native call returns.
        ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource);
    }
    *before = after;
}

/// Recorded but not submitted work. Dropping this value cancels immediately;
/// every native object remains private and has never been used by the GPU.
pub struct D3D12PreparedGraph {
    work: FrameWork,
}

impl D3D12Runtime {
    /// Records a compiled Graph without submitting or waiting. Input validity
    /// and ordering come from the shared immutable compiled Graph.
    pub fn prepare(&self, graph: &CompiledGraph) -> Result<D3D12PreparedGraph, D3D12RuntimeError> {
        Ok(D3D12PreparedGraph {
            work: bounded::prepare(&self.pool, graph)?,
        })
    }

    /// Convenience preparation and asynchronous submission; no GPU wait occurs.
    pub fn submit(&mut self, graph: &CompiledGraph) -> Result<D3D12Submission, D3D12RuntimeError> {
        let prepared = self.prepare(graph)?;
        self.submit_prepared(prepared)
    }

    /// Submits an owned recording. A recording from another Device owner is
    /// rejected before any queue command. Handles sharing the same native
    /// Device owner can submit each other's recordings. Execution and its
    /// completion signal are serialized across those handles.
    pub fn submit_prepared(
        &mut self,
        prepared: D3D12PreparedGraph,
    ) -> Result<D3D12Submission, D3D12RuntimeError> {
        let work = prepared.work;
        if !Arc::ptr_eq(&self.device, work.owner()) {
            return Err(D3D12RuntimeError::wrong_owner(
                "prepared Graph belongs to another D3D12 device",
            ));
        }
        Ok(D3D12Submission {
            submission: work.submit()?,
        })
    }
}

/// A prepared-capacity submission. Pending Drop detaches ownership for polling;
/// it does not wait, destroy resources, or increase the prepared capacity.
/// Export metadata remains retained after completion until this submission is
/// dropped. Return it before changing the runtime's working set.
pub struct D3D12Submission {
    submission: FrameSubmission,
}
impl D3D12Submission {
    /// Snapshot of accepted scheduled contents; completion is observed separately.
    /// Dropping it does not unregister the resource; use [`D3D12Runtime::delete`].
    /// Return this snapshot and all clones before changing the working set, then
    /// use [`D3D12Runtime::refresh_buffer`] with its retained key.
    pub fn export_buffer(
        &self,
        handle: crate::graph::BufferHandle,
    ) -> Result<crate::graph::ImportBuffer, D3D12RuntimeError> {
        self.submission
            .exports
            .as_ref()
            .ok_or_else(|| D3D12RuntimeError::invalid("Graph has no exports"))?
            .buffer(handle)
    }
    /// Snapshot of accepted scheduled contents; completion is observed separately.
    /// Dropping it does not unregister the resource; use [`D3D12Runtime::delete`].
    /// Image snapshots contain no pooled backing and may survive working-set changes.
    pub fn export_image(
        &self,
        handle: crate::graph::ImageHandle,
    ) -> Result<crate::graph::ImportImage, D3D12RuntimeError> {
        self.submission
            .exports
            .as_ref()
            .ok_or_else(|| D3D12RuntimeError::invalid("Graph has no exports"))?
            .image(handle)
    }
    pub fn poll(&mut self) -> Result<Option<D3D12SubmissionResult>, D3D12RuntimeError> {
        Ok(self
            .submission
            .poll()?
            .map(|result| D3D12SubmissionResult { result }))
    }
    pub fn wait(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<D3D12SubmissionResult>, D3D12RuntimeError> {
        Ok(self
            .submission
            .wait(timeout)?
            .map(|result| D3D12SubmissionResult { result }))
    }
}

/// CPU readback ownership is separate from reusable command execution slots.
pub struct D3D12SubmissionResult {
    result: FrameResult,
}
impl D3D12SubmissionResult {
    /// Optional completed CPU phases and DIRECT-queue Graph GPU interval.
    /// Present work and display/VSync completion are separate measurements.
    pub fn metrics(&self) -> Option<&crate::profiling::SubmissionMetrics> {
        self.result.metrics()
    }
    fn check_graph(&self, pass: PassHandle) -> Result<(), D3D12RuntimeError> {
        if self.result.graph() != pass.graph {
            return Err(D3D12RuntimeError::wrong_owner(
                "foreign Graph readback handle",
            ));
        }
        Ok(())
    }
    pub fn readback_len(&self, pass: PassHandle) -> Result<usize, D3D12RuntimeError> {
        self.check_graph(pass)?;
        self.result.readback_len(pass.index)
    }
    /// Copies into already prepared caller storage and consumes this readback.
    /// A wrong destination length or native failure leaves the lease available.
    pub fn readback_into(
        &mut self,
        pass: PassHandle,
        destination: &mut [u8],
    ) -> Result<(), D3D12RuntimeError> {
        self.check_graph(pass)?;
        self.result.readback_into(pass.index, destination)
    }
    #[cfg(test)]
    pub(super) fn readback(&mut self, pass: PassHandle) -> Result<Vec<u8>, D3D12RuntimeError> {
        let mut bytes = vec![0; self.readback_len(pass)?];
        self.readback_into(pass, &mut bytes)?;
        Ok(bytes)
    }
    #[cfg(all(test, feature = "d3d12-diagnostics"))]
    fn has_owner(&self) -> bool {
        self.result.has_owner()
    }
}

#[cfg(test)]
mod tests {
    use super::super::resources::MAX_BUFFER_BYTES;
    use super::*;
    use crate::graph::{BufferDesc, Graph, GraphCapacity, GraphPool, ImageDesc};

    fn prepared_graph() -> (GraphPool, Graph) {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let graph = pool.begin().unwrap();
        (pool, graph)
    }

    #[cfg(feature = "d3d12-diagnostics")]
    fn copy_graph() -> (GraphPool, CompiledGraph, [PassHandle; 3], PassHandle) {
        let (_graph_pool, mut graph) = prepared_graph();
        let a = graph.buffer("a", BufferDesc { size: 32 }).unwrap();
        let b = graph.buffer("b", BufferDesc { size: 32 }).unwrap();
        let bytes: [u8; 24] = std::array::from_fn(|index| index as u8);
        let upload = graph.upload_buffer("upload", a, 4, &bytes).unwrap();
        graph.copy_buffer("copy", a, 8, b, 12, 12).unwrap();
        let copied = graph.readback_buffer("copied", b, 12, 12).unwrap();
        let source = graph.readback_buffer("source", a, 4, 8).unwrap();
        let tail = graph.readback_buffer("tail", b, 16, 8).unwrap();
        // Ensure native recording order differs from pass-index order; result
        // lookup must work after completed entries are indexed independently.
        graph.depends_on(copied, tail).unwrap();
        (
            _graph_pool,
            graph.compile().unwrap(),
            [copied, source, tail],
            upload,
        )
    }

    fn requirements(graph: &CompiledGraph) -> Result<usize, D3D12RuntimeError> {
        let mut mapping = vec![usize::MAX; graph.resources.len()];
        bounded::requirements(
            graph,
            &mut mapping,
            super::super::D3D12FrameCapacity::default(),
        )
    }

    #[test]
    fn plan_uses_shared_culled_order_and_ignores_unused_resources() {
        let (_graph_pool, mut graph) = prepared_graph();
        let image = graph.image("unused image", ImageDesc::rgba8(1, 1)).unwrap();
        graph
            .upload_image("culled image upload", image, &[0; 4])
            .unwrap();
        let buffer = graph.buffer("buffer", BufferDesc { size: 4 }).unwrap();
        graph.upload_buffer("upload", buffer, 0, &[1; 4]).unwrap();
        graph.readback_buffer("readback", buffer, 0, 4).unwrap();
        let graph = graph.compile().unwrap();
        let mut mapping = [0; 4];
        let capacity = super::super::D3D12FrameCapacity::default();
        assert_eq!(
            bounded::requirements(&graph, &mut mapping, capacity).unwrap(),
            1
        );
        assert_eq!(&mapping[..2], &[usize::MAX, 0]);

        let (_graph_pool, mut smaller) = prepared_graph();
        let buffer = smaller.buffer("small", BufferDesc { size: 4 }).unwrap();
        smaller.upload_buffer("upload", buffer, 0, &[1; 4]).unwrap();
        smaller.readback_buffer("read", buffer, 0, 4).unwrap();
        assert_eq!(
            bounded::requirements(&smaller.compile().unwrap(), &mut mapping, capacity).unwrap(),
            1
        );
        assert_eq!(mapping[0], 0);
        assert_eq!(
            bounded::requirements(&graph, &mut mapping, capacity).unwrap(),
            1
        );
        assert_eq!(&mapping[..2], &[usize::MAX, 0]);
        assert_eq!(
            bounded::requirements(
                &graph,
                &mut mapping,
                super::super::D3D12FrameCapacity {
                    upload_bytes: 3,
                    ..capacity
                }
            )
            .unwrap_err()
            .kind(),
            super::super::D3D12RuntimeErrorKind::CapacityExhausted
        );
    }

    #[test]
    fn plan_accepts_exports_and_rejects_backend_capacity_before_native_allocation() {
        let (_graph_pool, mut graph) = prepared_graph();
        let buffer = graph.buffer("export", BufferDesc { size: 4 }).unwrap();
        graph.upload_buffer("upload", buffer, 0, &[1; 4]).unwrap();
        graph.export_buffer(buffer).unwrap();
        assert!(requirements(&graph.compile().unwrap()).is_ok());

        let (_graph_pool, mut graph) = prepared_graph();
        let buffer = graph
            .buffer(
                "large",
                BufferDesc {
                    size: MAX_BUFFER_BYTES + 4,
                },
            )
            .unwrap();
        graph.upload_buffer("upload", buffer, 0, &[1; 4]).unwrap();
        graph.readback_buffer("readback", buffer, 0, 4).unwrap();
        assert!(requirements(&graph.compile().unwrap()).is_err());
    }

    #[test]
    fn plan_rejects_live_image_work() {
        let mut graph = Graph::test_new();
        let image = graph.image("image", ImageDesc::rgba8(1, 1)).unwrap();
        graph.upload_image("upload image", image, &[0; 4]).unwrap();
        graph.readback_image("readback image", image).unwrap();
        assert!(requirements(&graph.compile().unwrap()).is_err());

        let source = crate::graph::ImportBuffer::new(
            crate::graph::ResourceKey {
                owner: 1,
                index: 0,
                generation: 0,
            },
            0,
            BufferDesc { size: 4 },
            crate::graph::ImportRanges::test_capture(std::slice::from_ref(&(0..4))),
        )
        .unwrap();
        let (_graph_pool, mut graph) = prepared_graph();
        let imported = graph.import_buffer("external buffer", &source).unwrap();
        graph
            .readback_buffer("readback external", imported, 0, 4)
            .unwrap();
        // Native-independent preflight admits imports. The mutable registry
        // checks their owner/generation/revision at preparation and submission.
        assert!(requirements(&graph.compile().unwrap()).is_ok());
    }

    #[test]
    #[ignore = "requires real D3D12 hardware; root serializes native acceptance"]
    fn d3d12_final_common_batch_records_once_and_recovers_after_capacity_rejection() {
        use super::super::{
            D3D12BufferClass, D3D12FrameCapacity, D3D12ImageClass, D3D12RuntimeConfig,
            D3D12ValidationMode,
        };
        let mut runtime = D3D12Runtime::new_with_working_set(
            D3D12RuntimeConfig {
                validation: if cfg!(feature = "d3d12-diagnostics") {
                    D3D12ValidationMode::Required
                } else {
                    D3D12ValidationMode::Disabled
                },
                capacity: D3D12FrameCapacity {
                    executions: 1,
                    results: 1,
                    resources: 8,
                    resource_capacity: 8,
                    operations: 16,
                    buffers: 2,
                    buffer_bytes: 32,
                    upload_bytes: 2048,
                    readback_bytes: 2048,
                    scratch_bytes: 32,
                    ..Default::default()
                },
            },
            &[D3D12BufferClass { size: 32, count: 2 }],
            &[D3D12ImageClass {
                desc: ImageDesc::rgba8(1, 1),
                count: 1,
            }],
            &[],
            &[],
        )
        .unwrap();
        let monitor = runtime.validation_monitor();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let a = graph.buffer("source", BufferDesc { size: 4 }).unwrap();
        let b = graph.buffer("destination", BufferDesc { size: 4 }).unwrap();
        graph.upload_buffer("upload", a, 0, &[1, 2, 3, 4]).unwrap();
        graph.copy_buffer("copy", a, 0, b, 0, 4).unwrap();
        let bytes = graph.readback_buffer("bytes", b, 0, 4).unwrap();
        let image = graph.image("image", ImageDesc::rgba8(1, 1)).unwrap();
        graph
            .upload_image("pixels", image, &[5, 6, 7, 255])
            .unwrap();
        let pixels = graph.readback_image("pixels", image).unwrap();
        let graph = graph.compile().unwrap();
        let mut canceled = runtime.prepare(&graph).unwrap();
        let slot = canceled.work.slot_mut();
        assert_eq!(
            (
                slot.common_barriers.recorded_calls,
                slot.common_barriers.recorded_transitions
            ),
            (1, 3),
            "two active buffers and one image share one actual native call; COMMON scratch is omitted"
        );
        assert!(
            slot.states[..slot.buffers_used]
                .iter()
                .all(|&state| state == D3D12_RESOURCE_STATE_COMMON)
        );
        assert_eq!(slot.scratch_state, D3D12_RESOURCE_STATE_COMMON);
        assert!(
            slot.raster
                .as_ref()
                .unwrap()
                .images
                .iter()
                .filter(|image| image.used)
                .all(|image| image.state == D3D12_RESOURCE_STATE_COMMON)
        );
        // Private unsubmitted planned states simulate a late capacity rejection;
        // the closed list is never submitted and canonical storage stays COMMON.
        slot.states[0] = D3D12_RESOURCE_STATE_COPY_DEST;
        let previous = slot.states.clone();
        slot.common_barriers.prepare_capacity(0).unwrap();
        let list: ID3D12GraphicsCommandList = slot.list.clone();
        assert!(transition_to_common(slot, &list).is_err());
        assert_eq!(slot.states, previous);
        assert_eq!(slot.common_barriers.recorded_calls, 0);
        slot.common_barriers
            .prepare_capacity(
                common_capacity(
                    slot.buffers.len(),
                    slot.raster.as_ref().unwrap().images.len(),
                )
                .unwrap(),
            )
            .unwrap();
        drop(canceled);
        let mut submission = runtime.submit(&graph).unwrap();
        let mut result = submission.wait(Duration::from_secs(30)).unwrap().unwrap();
        let mut data = [0; 4];
        result.readback_into(bytes, &mut data).unwrap();
        assert_eq!(data, [1, 2, 3, 4]);
        result.readback_into(pixels, &mut data).unwrap();
        assert_eq!(data, [5, 6, 7, 255]);
        drop((result, submission, graph, pool, list));
        runtime.shutdown().unwrap();
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(
            report.teardown_checked,
            cfg!(feature = "d3d12-diagnostics"),
            "{report:?}"
        );
        if cfg!(feature = "d3d12-diagnostics") {
            assert!(
                report.debug_layer_enabled && report.gpu_based_validation_enabled,
                "{report:?}"
            );
        }
    }

    #[test]
    #[ignore = "requires real D3D12 hardware, Debug Layer and GBV"]
    #[cfg(feature = "d3d12-diagnostics")]
    fn buffer_graph_lifetimes_partial_creation_and_pending_drop() {
        use super::super::{D3D12RuntimeConfig, D3D12RuntimeErrorKind, D3D12ValidationMode};
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: D3D12ValidationMode::Required,
            ..Default::default()
        })
        .expect("real D3D12 hardware and strict validation prerequisites");
        let monitor = runtime.validation_monitor();
        let weak = Arc::downgrade(&runtime.device);
        let (_graph_pool, graph, [copied, source, tail], upload) = copy_graph();
        let baseline = Arc::strong_count(&runtime.device);

        let canceled = runtime.prepare(&graph).expect("recorded work");
        assert!(weak.upgrade().is_some());
        drop(canceled);
        assert_eq!(
            Arc::strong_count(&runtime.device),
            baseline,
            "unsubmitted owner released"
        );

        // This fault happens before command recording; one real buffer is
        // sufficient to verify cleanup without a second Graph lowering path.
        #[cfg(feature = "memory-profiling")]
        let committed_before = runtime.memory_stats().unwrap().committed;
        let error = (|| -> Result<(), D3D12RuntimeError> {
            let _first = super::super::resources::DeviceBuffer::storage(&runtime.device, 4)?;
            Err(runtime.device.native(
                "injected next committed resource creation",
                windows_core::Error::from_hresult(windows_core::HRESULT(0x8007_000e_u32 as i32)),
            ))
        })()
        .unwrap_err();
        #[cfg(feature = "memory-profiling")]
        assert_eq!(runtime.memory_stats().unwrap().committed, committed_before);
        assert_eq!(error.kind(), D3D12RuntimeErrorKind::Native);
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(
            Arc::strong_count(&runtime.device),
            baseline,
            "partial creation released owner"
        );

        let first = runtime.submit(&graph).expect("first async submit");
        let mut second = runtime.submit(&graph).expect("second async submit");
        let deferred = runtime.shutdown().expect("runtime handle shutdown");
        assert!(
            !deferred.teardown_checked,
            "pending work retains native device and diagnostics"
        );
        // Waiting the second before the first does not release the first's
        // private command allocator, resources, Fence or Device owner.
        let mut result = second
            .wait(Duration::from_secs(30))
            .unwrap()
            .expect("second completion");
        let (_foreign_pool, mut foreign) = prepared_graph();
        let buffer = foreign.buffer("foreign", BufferDesc { size: 4 }).unwrap();
        foreign.upload_buffer("upload", buffer, 0, &[0; 4]).unwrap();
        let foreign_readback = foreign.readback_buffer("readback", buffer, 0, 4).unwrap();
        assert_eq!(
            result.readback(foreign_readback).unwrap_err().kind(),
            D3D12RuntimeErrorKind::WrongOwner
        );
        assert_eq!(
            result.readback(upload).unwrap_err().kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        // Consume independently of declaration order. Failed API calls must
        // neither consume another readback nor release its native owner.
        assert_eq!(
            result.readback(source).unwrap(),
            (0u8..8).collect::<Vec<_>>()
        );
        assert_eq!(
            result.readback(source).unwrap_err().kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        assert!(result.has_owner(), "unconsumed readbacks retain the owner");
        assert_eq!(
            result.readback(copied).unwrap(),
            (4u8..16).collect::<Vec<_>>()
        );
        assert_eq!(
            result.readback(tail).unwrap(),
            (8u8..16).collect::<Vec<_>>()
        );
        assert!(!result.has_owner(), "last readback releases the owner");
        assert_eq!(
            result.readback(copied).unwrap_err().kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        drop(result);
        drop(second);
        assert!(
            weak.upgrade().is_some(),
            "first submission still owns native dependencies"
        );
        drop(first); // Bounded wait for this submission, then exact retirement.
        assert!(
            weak.upgrade().is_none(),
            "last pending owner released after real completion"
        );
        let report = monitor.snapshot();
        assert!(report.debug_layer_enabled && report.gpu_based_validation_enabled);
        assert!(report.teardown_checked && report.is_clean(), "{report:?}");

        // An empty completed result owns no native readback. Its work's child
        // COM references must be released before the last diagnostic owner.
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: D3D12ValidationMode::Required,
            ..Default::default()
        })
        .unwrap();
        let monitor = runtime.validation_monitor();
        let (_empty_pool, empty_graph) = prepared_graph();
        let mut pending = runtime.submit(&empty_graph.compile().unwrap()).unwrap();
        assert!(!runtime.shutdown().unwrap().teardown_checked);
        let empty_result = pending.wait(Duration::from_secs(30)).unwrap().unwrap();
        let report = monitor.snapshot();
        assert!(report.teardown_checked && report.is_clean(), "{report:?}");
        drop(empty_result);
    }
}

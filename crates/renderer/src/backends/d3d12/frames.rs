use std::time::Duration;

use crate::{
    capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind},
    graph::{BufferHandle, CompiledGraph, ImageHandle, ImportBuffer, ImportImage},
    runtime::{FrameId, FrameQueue, Observation},
};

use super::registry::ExportLease;
use super::{D3D12Runtime, D3D12RuntimeError as Error, D3D12Submission, D3D12SubmissionResult};

pub(crate) struct QueuedFrameSubmission {
    pub(crate) id: FrameId,
    graph: u64,
    exports: Option<ExportLease>,
}

impl QueuedFrameSubmission {
    pub(crate) fn graph_id(&self) -> u64 {
        self.graph
    }

    fn exports(&self, graph: u64) -> Result<&ExportLease, Error> {
        // An existing export lease checks its own Graph. With no exports, keep
        // wrong-owner distinct from a missing export on the correct Graph.
        self.exports.as_ref().ok_or_else(|| {
            if self.graph == graph {
                Error::invalid("Graph has no exports")
            } else {
                Error::wrong_owner("foreign export Graph")
            }
        })
    }

    pub(crate) fn export_buffer(&self, handle: BufferHandle) -> Result<ImportBuffer, Error> {
        self.exports(handle.graph)?.buffer(handle)
    }

    pub(crate) fn export_image(&self, handle: ImageHandle) -> Result<ImportImage, Error> {
        self.exports(handle.graph)?.image(handle)
    }
}

pub(crate) struct QueuedFrameCompletion {
    pub(crate) id: FrameId,
    pub(crate) result: D3D12SubmissionResult,
}

struct PendingFrame {
    id: FrameId,
    submission: D3D12Submission,
}

impl PendingFrame {
    fn observe(&mut self, timeout: Duration) -> Observation<QueuedFrameCompletion, Error> {
        match self.submission.wait(timeout) {
            Ok(None) => Observation::Pending,
            Ok(Some(result)) => Observation::Complete(QueuedFrameCompletion {
                id: self.id,
                result,
            }),
            // An error before proving completion keeps the pending lease. Once
            // the marker is known complete, failure (e.g. invalid result from
            // a stopped Device) can release the frame without exposing data.
            Err(error) => Observation::Failed {
                error,
                retired: !self.submission.is_pending(),
            },
        }
    }
}

pub(super) struct QueuedFrames {
    owner: u64,
    serial: u64,
    pending: FrameQueue<PendingFrame>,
}

impl QueuedFrames {
    pub(super) fn new(limit: usize) -> Result<Self, Error> {
        let pending = FrameQueue::new(limit).map_err(|_| {
            Error::capacity_with(
                "reserving bounded frame bookkeeping",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::HostMemory,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            )
        })?;
        Ok(Self {
            owner: crate::graph::next_id(),
            serial: 0,
            pending,
        })
    }
}

impl D3D12Runtime {
    pub(crate) fn submit_frame(
        &mut self,
        graph: &CompiledGraph,
    ) -> Result<QueuedFrameSubmission, Error> {
        if self.frames.pending.is_full() {
            return Err(Error::capacity_with(
                "in-flight frame capacity exhausted; poll or explicitly wait before retrying",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::ExecutionSlots,
                    CapacityRecovery::Poll,
                    Some(self.frames.pending.limit() as u64),
                    None,
                ),
            ));
        }
        let serial = self
            .frames
            .serial
            .checked_add(1)
            .ok_or_else(|| Error::invalid("frame serial exhausted"))?;
        let mut submission = self.submit(graph)?;
        let id = FrameId::new(self.frames.owner, serial);
        let exports = submission.take_exports();
        self.frames.pending.push(PendingFrame { id, submission });
        self.frames.serial = serial;
        Ok(QueuedFrameSubmission {
            id,
            graph: graph.graph_id(),
            exports,
        })
    }

    pub(crate) fn pending_frames(&self) -> usize {
        self.frames.pending.len()
    }

    pub(crate) fn wait_frame(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<QueuedFrameCompletion>, Error> {
        self.frames.pending.wait_one(timeout, PendingFrame::observe)
    }

    pub(crate) fn wait_for_frame(
        &mut self,
        id: FrameId,
        timeout: Duration,
    ) -> Result<Option<QueuedFrameCompletion>, Error> {
        if id.owner != self.frames.owner {
            return Err(Error::wrong_owner("frame belongs to another runtime"));
        }
        self.frames.pending.wait_for(
            |frame| frame.id == id,
            |frame| frame.observe(timeout),
            || Error::invalid("frame is no longer pending"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        d3d12::{D3D12BufferClass, D3D12FrameCapacity, D3D12RuntimeConfig, D3D12RuntimeErrorKind},
        graph::{BufferDesc, Graph},
    };

    #[test]
    #[ignore = "requires a hardware D3D12 device; run in strict and shipping builds"]
    fn d3d12_runtime_owns_pending_frames_and_readback_leases_separately() {
        const WAIT: Duration = Duration::from_secs(10);
        let mut runtime = D3D12Runtime::new_with_working_set(
            D3D12RuntimeConfig {
                capacity: D3D12FrameCapacity {
                    executions: 2,
                    results: 2,
                    resources: 4,
                    resource_capacity: 4,
                    operations: 4,
                    buffers: 2,
                    buffer_bytes: 64,
                    upload_bytes: 64,
                    readback_bytes: 64,
                    scratch_bytes: 64,
                    ..Default::default()
                },
                ..Default::default()
            },
            &[D3D12BufferClass { size: 16, count: 2 }],
            &[],
            &[],
            &[],
        )
        .expect("hardware D3D12 runtime is required");
        let monitor = runtime.validation_monitor();

        let data = [17, 29, 83, 211, 27, 38, 11, 67];
        let mut producer = Graph::test_new();
        let buffer = producer.buffer("buffer", BufferDesc { size: 8 }).unwrap();
        producer.upload_buffer("upload", buffer, 0, &data).unwrap();
        let producer_readback = producer.readback_buffer("read", buffer, 0, 8).unwrap();
        producer.export_buffer(buffer).unwrap();
        let producer = producer.compile().unwrap();

        let accepted = runtime.submit_frame(&producer).unwrap();
        let first = accepted.id;
        let snapshot = accepted.export_buffer(buffer).unwrap();
        drop(accepted);
        assert_eq!(runtime.pending_frames(), 1);

        let mut consumer = Graph::test_new();
        let imported = consumer.import_buffer("scheduled", &snapshot).unwrap();
        let consumer_readback = consumer.readback_buffer("read", imported, 0, 8).unwrap();
        let consumer = consumer.compile().unwrap();
        let accepted = runtime.submit_frame(&consumer).unwrap();
        let second = accepted.id;
        assert_eq!(
            accepted.export_buffer(buffer).unwrap_err().kind(),
            D3D12RuntimeErrorKind::WrongOwner,
        );
        assert_eq!(
            accepted.export_buffer(imported).unwrap_err().kind(),
            D3D12RuntimeErrorKind::InvalidInput,
        );
        drop(accepted);
        runtime.delete(snapshot.key()).unwrap();
        drop(snapshot);

        let error = runtime.submit_frame(&producer).err().unwrap();
        let capacity = error.capacity().unwrap();
        assert_eq!(capacity.resource, RuntimeCapacityKind::ExecutionSlots);
        assert_eq!(capacity.recovery, CapacityRecovery::Poll);
        assert_eq!(runtime.pending_frames(), 2);

        let foreign = FrameId::new(crate::graph::next_id(), 1);
        let error = runtime.wait_for_frame(foreign, WAIT).err().unwrap();
        assert_eq!(error.kind(), D3D12RuntimeErrorKind::WrongOwner);

        let mut second_result = runtime.wait_for_frame(second, WAIT).unwrap().unwrap();
        assert_eq!(second_result.id, second);
        assert_eq!(runtime.pending_frames(), 1);
        let error = runtime.wait_for_frame(second, WAIT).err().unwrap();
        assert_eq!(error.kind(), D3D12RuntimeErrorKind::InvalidInput);
        let mut first_result = runtime.wait_frame(WAIT).unwrap().unwrap();
        assert_eq!(first_result.id, first);
        assert_eq!(runtime.pending_frames(), 0);

        let error = runtime.submit_frame(&producer).err().unwrap();
        let capacity = error.capacity().unwrap();
        assert_eq!(capacity.resource, RuntimeCapacityKind::ResultSlots);
        assert_eq!(capacity.recovery, CapacityRecovery::ReleaseResults);

        let mut received = [0; 8];
        second_result
            .result
            .readback_into(consumer_readback, &mut received)
            .unwrap();
        assert_eq!(received, data);
        let third = runtime.submit_frame(&producer).unwrap();
        runtime
            .delete(third.export_buffer(buffer).unwrap().key())
            .unwrap();
        assert_eq!(
            runtime.wait_for_frame(third.id, WAIT).unwrap().unwrap().id,
            third.id
        );
        first_result
            .result
            .readback_into(producer_readback, &mut received)
            .unwrap();
        assert_eq!(received, data);
        drop((third, first_result, second_result, runtime));

        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:?}");
        if cfg!(feature = "development-validation") {
            assert!(
                report.debug_layer_enabled && report.gpu_based_validation_enabled,
                "{report:?}"
            );
            assert!(report.teardown_checked, "{report:?}");
        }
    }
}

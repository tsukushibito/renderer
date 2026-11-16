//! Opt-in timestamp pairs reserved at explicit preparation boundaries.

use std::time::{Duration, Instant};

use crate::profiling::{GpuTiming, SubmissionMetrics};

use super::{
    D3D12Runtime, D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{NativeObject, object},
    resources::DeviceBuffer,
};

impl D3D12Runtime {
    /// Enable optional CPU phases and DIRECT-queue GPU timestamps. The first
    /// enable reserves query/readback storage and requires idle execution slots.
    /// Disable affects future recordings; accepted samples remain valid.
    pub fn set_profiling(&mut self, enabled: bool) -> Result<(), D3D12RuntimeError> {
        self.pool.set_profiling(enabled)
    }

    pub fn profiling_enabled(&self) -> bool {
        self.pool.profiling_enabled()
    }
}

/// One pair belongs to exactly one execution/presentation slot. Reuse requires
/// successful GPU completion, including the resolve into its persistent readback.
pub(super) struct TimestampPair {
    heap: NativeObject<ID3D12QueryHeap>,
    readback: DeviceBuffer,
    frequency: u64,
}

impl TimestampPair {
    pub(super) fn new(owner: &DeviceOwner) -> Result<Self, D3D12RuntimeError> {
        owner.ensure_active()?;
        // SAFETY: The live DIRECT queue always supports timestamp queries. Its
        // own frequency defines the timebase for commands submitted to it.
        let frequency = unsafe { owner.queue.GetTimestampFrequency() }
            .map_err(|error| owner.native("ID3D12CommandQueue::GetTimestampFrequency", error))?;
        if frequency == 0 {
            return Err(D3D12RuntimeError::unsupported(
                "queue timestamp frequency is zero",
            ));
        }
        let desc = D3D12_QUERY_HEAP_DESC {
            Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
            Count: 2,
            NodeMask: 0,
        };
        let mut heap = None;
        // SAFETY: The two TIMESTAMP entries match the baseline DIRECT queue;
        // the output is local and immediately enters an owning wrapper.
        let heap = object(owner, "ID3D12Device::CreateQueryHeap", || unsafe {
            owner.raw.CreateQueryHeap(&desc, &mut heap)?;
            heap.ok_or_else(|| {
                windows_core::Error::from_hresult(windows_core::HRESULT(0x8000_4003u32 as i32))
            })
        })?;
        let mut readback = DeviceBuffer::new(owner, 16, D3D12_HEAP_TYPE_READBACK)?;
        readback.map_persistent(owner, 16, true)?;
        Ok(Self {
            heap,
            readback,
            frequency,
        })
    }

    /// # Safety
    /// The matching DIRECT list is exclusively recorded on this Device, and
    /// this pair is not referenced by another pending/recorded execution.
    pub(super) unsafe fn begin(&self, list: &ID3D12GraphicsCommandList) {
        // SAFETY: TIMESTAMP is an EndQuery-only operation. Every reuse is
        // ordered after the prior execution/resolve and CPU consumption.
        unsafe {
            list.EndQuery(&*self.heap, D3D12_QUERY_TYPE_TIMESTAMP, 0);
        }
    }

    /// # Safety
    /// The matching begin and all measured work precede this call. Retain the
    /// pair and its readback until actual successful completion is established.
    pub(super) unsafe fn end(&self, list: &ID3D12GraphicsCommandList) {
        // SAFETY: Both timestamp entries are initialized in this execution.
        // READBACK resources remain COPY_DEST, and the 16-byte output at zero
        // meets UINT64 alignment and resolves only this pair's valid entries.
        unsafe {
            list.EndQuery(&*self.heap, D3D12_QUERY_TYPE_TIMESTAMP, 1);
            list.ResolveQueryData(
                &*self.heap,
                D3D12_QUERY_TYPE_TIMESTAMP,
                0,
                2,
                self.readback.raw(),
                0,
            );
        }
    }

    /// # Safety
    /// A successful Fence observation established completion of both timestamp
    /// writes and resolve, with no concurrent query/readback reuse or CPU read.
    pub(super) unsafe fn read(
        &self,
        owner: &DeviceOwner,
        host_lifetime: Duration,
    ) -> Result<GpuTiming, D3D12RuntimeError> {
        let mut bytes = [0u8; 16];
        // SAFETY: Caller proves completion and exclusive access to all 16 bytes.
        unsafe { self.readback.read_into_at(owner, 0, &mut bytes) }?;
        Ok(counter_interval(
            u64::from_ne_bytes(bytes[..8].try_into().unwrap()),
            u64::from_ne_bytes(bytes[8..].try_into().unwrap()),
            self.frequency,
            host_lifetime,
        ))
    }
}

fn counter_interval(start: u64, end: u64, frequency: u64, host_lifetime: Duration) -> GpuTiming {
    let period_ns = 1e9 / frequency as f64;
    if host_lifetime.as_secs_f64() >= 2_f64.powi(64) / frequency as f64 {
        return GpuTiming::AmbiguousCounterWrap;
    }
    let Ok(elapsed) =
        Duration::try_from_secs_f64(end.wrapping_sub(start) as f64 / frequency as f64)
    else {
        return GpuTiming::AmbiguousCounterWrap;
    };
    GpuTiming::Available {
        elapsed,
        timestamp_period_ns: period_ns,
        valid_bits: 64,
    }
}

pub(super) struct WorkProfile {
    pub(super) started: Instant,
    pub(super) recording: Option<Instant>,
    pub(super) metrics: SubmissionMetrics,
}
impl WorkProfile {
    pub(super) fn new(started: Instant) -> Self {
        Self {
            started,
            recording: None,
            metrics: SubmissionMetrics {
                cpu_prepare: Duration::ZERO,
                cpu_record: Duration::ZERO,
                cpu_submit: Duration::ZERO,
                cpu_retire: Duration::ZERO,
                gpu: GpuTiming::NotReady,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_ticks_keep_fractional_precision_and_support_one_counter_wrap() {
        assert_eq!(
            counter_interval(10, 12, 2_000_000_000, Duration::from_secs(1)),
            GpuTiming::Available {
                elapsed: Duration::from_nanos(1),
                timestamp_period_ns: 0.5,
                valid_bits: 64
            }
        );
        assert_eq!(
            counter_interval(u64::MAX - 2, 3, 1_000_000_000, Duration::from_secs(1)),
            GpuTiming::Available {
                elapsed: Duration::from_nanos(6),
                timestamp_period_ns: 1.0,
                valid_bits: 64
            }
        );
        assert_eq!(
            counter_interval(0, 0, u64::MAX, Duration::from_secs(2)),
            GpuTiming::AmbiguousCounterWrap
        );
    }

    #[test]
    #[ignore = "explicit real-GPU optional timestamp and CPU-result lifetime acceptance"]
    fn d3d12_graph_optional_metrics_survive_disable_and_reuse_with_held_readbacks() {
        use super::super::{D3D12FrameCapacity, D3D12RuntimeConfig, D3D12RuntimeErrorKind};
        use crate::graph::{BufferDesc, Graph};
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            capacity: D3D12FrameCapacity {
                executions: 1,
                results: 2,
                buffers: 1,
                buffer_bytes: 16,
                upload_bytes: 16,
                readback_bytes: 16,
                scratch_bytes: 16,
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("hardware D3D12 is required");
        let monitor = runtime.validation_monitor();
        let mut graph = Graph::test_new();
        let buffer = graph
            .buffer("timed buffer", BufferDesc { size: 16 })
            .unwrap();
        graph
            .upload_buffer("initialize", buffer, 0, &[37; 16])
            .unwrap();
        let read = graph.readback_buffer("read", buffer, 0, 16).unwrap();
        let graph = graph.compile().unwrap();
        assert!(!runtime.profiling_enabled());
        let mut unprofiled = runtime.submit(&graph).unwrap();
        assert_eq!(
            runtime.set_profiling(true).unwrap_err().kind(),
            D3D12RuntimeErrorKind::CapacityExhausted
        );
        let result = unprofiled.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert!(result.metrics().is_none());
        drop(result);
        drop(unprofiled);
        runtime.set_profiling(true).unwrap();
        // Cancel returns the open recording and its query pair without issuing
        // commands. A subsequent execution can overwrite the same pair safely.
        drop(runtime.prepare(&graph).unwrap());
        let mut first = runtime.submit(&graph).unwrap();
        let mut first_result = first.wait(Duration::from_secs(10)).unwrap().unwrap();
        let first_metrics = *first_result.metrics().expect("timing was requested");
        let mut second = runtime.submit(&graph).unwrap();
        runtime.set_profiling(false).unwrap();
        let mut second_result = second.wait(Duration::from_secs(10)).unwrap().unwrap();
        for sample in [first_metrics, *second_result.metrics().unwrap()] {
            assert!(
                matches!(sample.gpu, GpuTiming::Available { valid_bits:64, timestamp_period_ns, .. } if timestamp_period_ns>0.0),
                "{sample:?}"
            );
        }
        assert_eq!(
            first_result.metrics().unwrap().gpu,
            first_metrics.gpu,
            "reusing execution queries cannot mutate an earlier copied result"
        );
        let mut bytes = [0u8; 16];
        first_result.readback_into(read, &mut bytes).unwrap();
        assert_eq!(bytes, [37; 16]);
        second_result.readback_into(read, &mut bytes).unwrap();
        assert_eq!(bytes, [37; 16]);
        drop((first_result, second_result, first, second));
        runtime.set_profiling(true).unwrap();
        runtime
            .prepare_graph_working_set(None, &[], &[], &[])
            .unwrap();
        assert!(
            runtime.profiling_enabled(),
            "explicit working-set change preserves profiling request"
        );
        let mut replaced = runtime.submit(&graph).unwrap();
        let result = replaced.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert!(matches!(
            result.metrics().unwrap().gpu,
            GpuTiming::Available { .. }
        ));
        drop((result, replaced, runtime));
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:?}");
        #[cfg(feature = "development-validation")]
        assert!(
            report.debug_layer_enabled
                && report.gpu_based_validation_enabled
                && report.teardown_checked,
            "{report:?}"
        );
    }
}

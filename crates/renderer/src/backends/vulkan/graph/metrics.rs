//! Optional, bounded timestamp ownership and explicitly named memory snapshots.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ash::vk;

use super::{MemoryOwner, RuntimeError, VulkanRuntime};

/// GPU elapsed time for the whole recorded Graph, not display latency or FPS.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum GpuTiming {
    Available {
        elapsed: Duration,
        timestamp_period_ns: f64,
        valid_bits: u32,
    },
    /// A completed query was unexpectedly unavailable; no duration is inferred.
    NotReady,
    /// The sample's host lifetime cannot exclude multiple counter wraps.
    AmbiguousCounterWrap,
}

/// Optional CPU phases and the GPU Graph interval. Build/compile are caller work.
#[derive(Clone, Copy, Debug)]
pub struct SubmissionMetrics {
    /// Validation, resource preparation and lazy pipeline creation before record.
    pub cpu_prepare: Duration,
    /// Command recording, including its begin/end calls.
    pub cpu_record: Duration,
    /// Native submit and the Runtime's acceptance bookkeeping.
    pub cpu_submit: Duration,
    /// Completion bookkeeping, resource retirement and query result copying.
    /// Excludes time blocked in a Fence wait and unsuccessful polling attempts.
    pub cpu_retire: Duration,
    pub gpu: GpuTiming,
}

/// Requested payload and aligned reservations have distinct capacities/lifetimes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StagingMemoryStats {
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub peak_reserved_bytes: u64,
    pub live_ranges: usize,
}

/// A diagnostic snapshot, not process VRAM usage or a promise of future budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryStats {
    /// Buffer sizes plus packed image texel bytes of externally named resources.
    pub registered_logical_bytes: u64,
    /// Unregistered resources still leased by recorded/pending work.
    pub retained_logical_bytes: u64,
    /// Unregistered physical resources available for same-condition reuse.
    pub cached_logical_bytes: u64,
    pub upload: StagingMemoryStats,
    pub readback: StagingMemoryStats,
    pub vma_allocation_count: u32,
    pub vma_allocation_bytes: u64,
    pub vma_block_count: u32,
    pub vma_block_bytes: u64,
    /// EXT_memory_budget is not enabled by this baseline; do not report a VMA
    /// fallback estimate as measured process usage/budget.
    pub process_budget_bytes: Option<u64>,
}

impl VulkanRuntime {
    /// Enable optional timing. Ordinary construction and submission leave it off.
    /// A bounded query pool is created once, only after timestamp support checks.
    /// Disabling affects future submits; already accepted samples remain valid.
    pub fn set_profiling(&mut self, enabled: bool) -> Result<(), RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if enabled && self.timestamp_pool.is_none() {
            self.timestamp_pool = Some(TimestampPool::new(
                Arc::clone(&self.owner),
                self.config.execution_slots,
            )?);
        }
        self.profiling_enabled = enabled;
        Ok(())
    }

    pub fn profiling_enabled(&self) -> bool {
        self.profiling_enabled
    }

    /// Calculate VMA allocator statistics on demand, outside the normal hot path.
    /// Image logical bytes are packed texels, not optimal-image allocation sizes.
    /// VMA covers this allocator, excluding driver/Swapchain allocations.
    pub fn memory_stats(&self) -> Result<MemoryStats, RuntimeError> {
        let total = self.owner.raw.calculate_statistics().map_err(|result| {
            RuntimeError::native("vmaCalculateStatistics", self.device_name(), result)
        })?;
        let native = total.total.statistics;
        let convert = |stats: super::super::transfer::staging::StagingStats| StagingMemoryStats {
            capacity_bytes: stats.capacity,
            used_bytes: stats.used_bytes,
            reserved_bytes: stats.reserved_bytes,
            peak_reserved_bytes: stats.high_water_reserved_bytes,
            live_ranges: stats.live_ranges,
        };
        let mut output = MemoryStats {
            upload: convert(self.upload.stats()),
            readback: convert(self.readback.stats()),
            vma_allocation_count: native.allocationCount,
            vma_allocation_bytes: native.allocationBytes,
            vma_block_count: native.blockCount,
            vma_block_bytes: native.blockBytes,
            ..MemoryStats::default()
        };
        for entry in &self.entries {
            let Some(physical) = &entry.physical else {
                continue;
            };
            let bytes = match physical.desc {
                crate::graph::ResourceDesc::Buffer(desc) => desc.size,
                crate::graph::ResourceDesc::Image(desc) => desc.byte_len().unwrap_or(u64::MAX),
            };
            let target = if entry.registered {
                &mut output.registered_logical_bytes
            } else if Arc::strong_count(physical) > 1 {
                &mut output.retained_logical_bytes
            } else {
                &mut output.cached_logical_bytes
            };
            *target = target.saturating_add(bytes);
        }
        Ok(output)
    }
}

impl super::SubmissionResult {
    pub(super) fn completed(graph: u64, mut work: super::GraphWork) -> Result<Self, RuntimeError> {
        if let Some(profile) = &mut work.profile {
            profile.begin_retire();
        }
        // Recording uses readback vector offsets, so reorder only after actual
        // completion retired those operands. Pass identities are unique; every
        // result lookup can now use binary search without another retained index.
        work.readbacks.sort_unstable_by_key(|(index, _)| *index);
        if let Some(profile) = &mut work.profile {
            profile.end_retire();
            // SAFETY: Only the successful PendingTransfer wait/poll path calls
            // this constructor, after the actual Fence and active Device check.
            unsafe { profile.finish() }?;
        }
        Ok(Self { graph, work })
    }

    /// Available only when profiling was enabled for this accepted submission.
    pub fn metrics(&self) -> Option<&SubmissionMetrics> {
        self.work.profile.as_ref().map(|profile| &profile.result)
    }
}

pub(super) struct TimestampPool(Arc<TimestampOwner>);

struct TimestampOwner {
    owner: Arc<MemoryOwner>,
    pool: vk::QueryPool,
    valid_bits: u32,
    period_ns: f64,
    leased: Mutex<Vec<bool>>,
}

impl TimestampOwner {
    fn lock(&self) -> MutexGuard<'_, Vec<bool>> {
        match self.leased.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                self.owner.device.stop();
                self.leased.clear_poison();
                poisoned.into_inner()
            }
        }
    }
}

impl TimestampPool {
    fn new(owner: Arc<MemoryOwner>, capacity: usize) -> Result<Self, RuntimeError> {
        let count = capacity
            .checked_mul(2)
            .and_then(|count| u32::try_from(count).ok())
            .filter(|&count| count != 0)
            .ok_or_else(|| RuntimeError::invalid("timestamp query capacity exceeds u32"))?;
        let device = &owner.device;
        // SAFETY: Read-only properties on the physical adapter retained by owner.
        let families = unsafe {
            device
                .instance_raw()
                .get_physical_device_queue_family_properties(device.physical_device())
        };
        let valid_bits = families[device.queue_family() as usize].timestamp_valid_bits;
        // SAFETY: The physical adapter and its Instance remain live through owner.
        let properties = unsafe {
            device
                .instance_raw()
                .get_physical_device_properties(device.physical_device())
        };
        let period_ns = f64::from(properties.limits.timestamp_period);
        if !(1..=64).contains(&valid_bits) || !period_ns.is_finite() || period_ns <= 0.0 {
            return Err(RuntimeError::unsupported(
                "selected queue has no usable timestamp counter",
            ));
        }
        // SAFETY: Timestamp queries require no extra feature. Query count fits
        // u32, and the owner is established before another fallible operation.
        let pool = unsafe {
            device.raw().create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(count),
                None,
            )
        }
        .map_err(|result| {
            device.observe_result(result);
            RuntimeError::native("vkCreateQueryPool", device.name(), result)
        })?;
        Ok(Self(Arc::new(TimestampOwner {
            owner,
            pool,
            valid_bits,
            period_ns,
            leased: Mutex::new(vec![false; capacity]),
        })))
    }

    pub(super) fn acquire(&self) -> Result<TimestampLease, RuntimeError> {
        self.0
            .owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let mut slots = self.0.lock();
        let index = slots
            .iter()
            .position(|leased| !leased)
            .ok_or_else(|| RuntimeError::capacity("timestamp query capacity exhausted"))?;
        slots[index] = true;
        Ok(TimestampLease {
            pool: Arc::clone(&self.0),
            index: index as u32,
        })
    }
}

impl Drop for TimestampOwner {
    fn drop(&mut self) {
        // SAFETY: Every recorded/pending use retains a lease and this owner.
        // The last owner is released before submit, after actual completion,
        // or under Device-loss retirement. Uncertain work retains the Arc.
        unsafe { self.owner.device.raw().destroy_query_pool(self.pool, None) };
    }
}

pub(super) struct TimestampLease {
    pool: Arc<TimestampOwner>,
    index: u32,
}

impl TimestampLease {
    /// # Safety
    /// command records on this Device, outside rendering. This lease is retained
    /// by the same recording/pending owner through completion or cancellation.
    pub(super) unsafe fn begin(&self, command: vk::CommandBuffer) {
        let device = self.pool.owner.device.raw();
        // SAFETY: The unique pair is not in use by another recording/submission.
        // Queue reset orders both writes without requiring hostQueryReset.
        unsafe {
            device.cmd_reset_query_pool(command, self.pool.pool, self.index * 2, 2);
            device.cmd_write_timestamp2(
                command,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                self.pool.pool,
                self.index * 2,
            );
        }
    }

    /// # Safety
    /// The matching begin preceded all Graph commands on the retained command.
    pub(super) unsafe fn end(&self, command: vk::CommandBuffer) {
        // SAFETY: This is the other query in the uniquely leased pair, outside
        // rendering; BOTTOM_OF_PIPE includes preceding Graph execution stages.
        unsafe {
            self.pool.owner.device.raw().cmd_write_timestamp2(
                command,
                vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
                self.pool.pool,
                self.index * 2 + 1,
            );
        }
    }

    /// # Safety
    /// The whole recorded submit has completed with a successful Fence wait.
    unsafe fn read(&self, host_lifetime: Duration) -> Result<GpuTiming, RuntimeError> {
        let device = &self.pool.owner.device;
        let mut values = [[0_u64; 2]; 2];
        // SAFETY: Actual Fence completion proves both reset and writes executed.
        // ash uses the slice length as query count and each [u64;2] as stride.
        // Results and availability are copied without a new GPU wait.
        let result = unsafe {
            device.raw().get_query_pool_results(
                self.pool.pool,
                self.index * 2,
                &mut values,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WITH_AVAILABILITY,
            )
        };
        match result {
            Ok(()) if values[0][1] != 0 && values[1][1] != 0 => Ok(counter_interval(
                values[0][0],
                values[1][0],
                self.pool.valid_bits,
                self.pool.period_ns,
                host_lifetime,
            )),
            Ok(()) | Err(vk::Result::NOT_READY) => Ok(GpuTiming::NotReady),
            Err(result) => {
                device.observe_result(result);
                Err(RuntimeError::native(
                    "vkGetQueryPoolResults",
                    device.name(),
                    result,
                ))
            }
        }
    }
}

impl Drop for TimestampLease {
    fn drop(&mut self) {
        self.pool.lock()[self.index as usize] = false;
    }
}

fn counter_interval(
    start: u64,
    end: u64,
    valid_bits: u32,
    period_ns: f64,
    host_lifetime: Duration,
) -> GpuTiming {
    let wrap_ns = 2_f64.powi(valid_bits as i32) * period_ns;
    // A conservative upper bound includes preparation, queuing and result
    // retrieval. Samples this old cannot exclude more than one device wrap.
    if host_lifetime.as_secs_f64() * 1e9 >= wrap_ns {
        return GpuTiming::AmbiguousCounterWrap;
    }
    let mask = if valid_bits == 64 {
        u64::MAX
    } else {
        (1_u64 << valid_bits) - 1
    };
    let ticks = end.wrapping_sub(start) & mask;
    let Ok(elapsed) = Duration::try_from_secs_f64(ticks as f64 * period_ns / 1e9) else {
        return GpuTiming::AmbiguousCounterWrap;
    };
    GpuTiming::Available {
        elapsed,
        timestamp_period_ns: period_ns,
        valid_bits,
    }
}

pub(super) struct WorkProfile {
    pub(super) lease: Option<TimestampLease>,
    pub(super) cpu_pending: Arc<Mutex<[Duration; 2]>>,
    started: Instant,
    retire_started: Option<Instant>,
    pub(super) result: SubmissionMetrics,
}

impl WorkProfile {
    pub(super) fn new(lease: TimestampLease, started: Instant) -> Self {
        Self {
            lease: Some(lease),
            cpu_pending: Arc::new(Mutex::new([Duration::ZERO; 2])),
            started,
            retire_started: None,
            result: SubmissionMetrics {
                cpu_prepare: started.elapsed(),
                cpu_record: Duration::ZERO,
                cpu_submit: Duration::ZERO,
                cpu_retire: Duration::ZERO,
                gpu: GpuTiming::NotReady,
            },
        }
    }

    pub(super) fn begin_retire(&mut self) {
        self.retire_started = Some(Instant::now());
    }

    pub(super) fn end_retire(&mut self) {
        if let Some(started) = self.retire_started.take() {
            self.result.cpu_retire += started.elapsed();
        }
    }

    /// # Safety
    /// A successful Fence wait established execution completion on active Device.
    pub(super) unsafe fn finish(&mut self) -> Result<(), RuntimeError> {
        let started = Instant::now();
        let pending = self
            .cpu_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.result.cpu_record = pending[0];
        self.result.cpu_submit = pending[1];
        drop(pending);
        if let Some(lease) = self.lease.take() {
            // SAFETY: Caller proved completion; lease excludes reset/reuse until
            // the owned result is copied, after which it is returned immediately.
            self.result.gpu = unsafe { lease.read(self.started.elapsed()) }?;
        }
        self.result.cpu_retire += started.elapsed();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_difference_uses_valid_bits_and_rejects_ambiguous_age() {
        assert_eq!(
            counter_interval(0xffff_fff0, 0x1000_0010, 28, 2.0, Duration::from_millis(1)),
            GpuTiming::Available {
                elapsed: Duration::from_nanos(64),
                timestamp_period_ns: 2.0,
                valid_bits: 28,
            }
        );
        assert_eq!(
            counter_interval(u64::MAX - 2, 3, 64, 1.0, Duration::from_secs(1)),
            GpuTiming::Available {
                elapsed: Duration::from_nanos(6),
                timestamp_period_ns: 1.0,
                valid_bits: 64,
            }
        );
        assert_eq!(
            counter_interval(1, 2, 8, 1.0, Duration::from_nanos(256)),
            GpuTiming::AmbiguousCounterWrap
        );
    }

    #[test]
    #[ignore = "explicit real-GPU optional timestamp/query and memory lifetime test"]
    fn vulkan_graph_optional_metrics_reuse_queries_before_cpu_readback_consumption() {
        use crate::graph::{BufferDesc, Graph};
        use std::collections::HashMap;

        let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
        tracing::dispatcher::with_default(&dispatch, || {
            let mut runtime = VulkanRuntime::new(super::super::RuntimeConfig {
                execution_slots: 2,
                resource_capacity: 4,
                staging_bytes: 4096,
                max_shader_bindings: 2,
            })
            .expect("real Vulkan runtime is required");
            assert_ne!(
                runtime.owner.device.device_type(),
                vk::PhysicalDeviceType::CPU,
                "CPU fallback is not real-GPU acceptance"
            );
            let diagnostics = runtime.diagnostics.clone();
            crate::backends::vulkan::assert_real_validation(&diagnostics);

            let mut graph = Graph::new();
            let buffer = graph.buffer("unprofiled", BufferDesc { size: 16 }).unwrap();
            graph
                .upload_buffer("upload", buffer, 0, vec![9; 16])
                .unwrap();
            graph.readback_buffer("read", buffer, 0, 16).unwrap();
            let mut submission = runtime.submit(&graph.compile().unwrap()).unwrap();
            let result = submission.wait(Duration::from_secs(10)).unwrap().unwrap();
            assert!(result.metrics().is_none());
            assert!(
                runtime.timestamp_pool.is_none(),
                "ordinary submit creates no query pool"
            );
            drop(result);
            drop(submission);

            runtime.set_profiling(true).unwrap();
            let query_owner = Arc::clone(&runtime.timestamp_pool.as_ref().unwrap().0);
            let mut expected = HashMap::new();
            let mut results = Vec::new();
            for epoch in 0_u32..2 {
                for index in 0..2 {
                    let value = (epoch * 2 + index + 1) as u8;
                    let mut graph = Graph::new();
                    let buffer = graph.buffer("profiled", BufferDesc { size: 16 }).unwrap();
                    graph
                        .upload_buffer("upload", buffer, 0, vec![value; 16])
                        .unwrap();
                    let read = graph.readback_buffer("read", buffer, 0, 16).unwrap();
                    let frame = runtime.submit_frame(&graph.compile().unwrap()).unwrap();
                    expected.insert(frame.id(), (read, value));
                    drop(frame);
                }
                assert!(query_owner.lock().iter().all(|leased| *leased));
                runtime.set_profiling(false).unwrap();
                let pending_memory = runtime.memory_stats().unwrap();
                assert_eq!(pending_memory.upload.used_bytes, 32);
                assert_eq!(
                    pending_memory.readback.used_bytes,
                    u64::from(epoch + 1) * 32
                );
                assert_eq!(pending_memory.retained_logical_bytes, 32);
                let deadline = Instant::now() + Duration::from_secs(10);
                while runtime.pending_frames() != 0 {
                    if let Some(frame) = runtime.poll_frame().unwrap() {
                        let sample = frame
                            .result
                            .metrics()
                            .expect("accepted profiling survives disable");
                        match sample.gpu {
                            GpuTiming::Available {
                                timestamp_period_ns,
                                valid_bits,
                                ..
                            } => {
                                assert!(timestamp_period_ns > 0.0);
                                assert!((1..=64).contains(&valid_bits));
                            }
                            other => {
                                panic!("completed real-GPU queries must be readable: {other:?}")
                            }
                        }
                        results.push(frame);
                    } else {
                        assert!(Instant::now() < deadline, "GPU frames must complete");
                        std::thread::yield_now();
                    }
                }
                assert!(query_owner.lock().iter().all(|leased| !leased));
                let memory = runtime.memory_stats().unwrap();
                assert_eq!(memory.upload.used_bytes, 0);
                assert_eq!(memory.readback.used_bytes, u64::from(epoch + 1) * 32);
                assert_eq!(memory.cached_logical_bytes, 32);
                assert!(memory.vma_block_bytes >= memory.vma_allocation_bytes);
                assert!(memory.vma_allocation_bytes > 0);
                assert_eq!(memory.process_budget_bytes, None);
                runtime.set_profiling(true).unwrap();
                assert!(Arc::ptr_eq(
                    &query_owner,
                    &runtime.timestamp_pool.as_ref().unwrap().0
                ));
            }
            let expected_peak = runtime.memory_stats().unwrap().readback.reserved_bytes;
            for mut frame in results {
                let (read, value) = expected.remove(&frame.id()).unwrap();
                assert_eq!(frame.result.readback(read).unwrap(), vec![value; 16]);
            }
            assert!(expected.is_empty());
            let memory = runtime.memory_stats().unwrap();
            assert_eq!(memory.readback.used_bytes, 0);
            assert_eq!(memory.readback.peak_reserved_bytes, expected_peak);
            drop(query_owner);
            drop(runtime);
            let snapshot = diagnostics.snapshot();
            assert_eq!(snapshot.warnings, 0, "{snapshot:?}");
            assert_eq!(snapshot.errors, 0, "{snapshot:?}");
            assert_eq!(snapshot.callback_failures, 0, "{snapshot:?}");
        });
    }
}

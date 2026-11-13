//! Explicit release-only measurements, separate from validation acceptance.

use std::ffi::CStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ash::vk;

use super::super::diagnostics::Diagnostics;
use super::super::instance::{ValidationMode, VulkanInstance};
use super::super::memory::VulkanAllocator;
use super::batch::TransferPool;
use super::{
    BufferRole, ExecutionPool, MemoryOwner, RecordedTransfer, TransferBuffers, TransferError,
    TransferPayload, VulkanBuffer, VulkanDevice,
};

const BYTES: usize = 1024 * 1024;
const WARMUP: usize = 20;
const SAMPLES: usize = 200;

/// The timer moves with the submitted work. An unresolved submission therefore
/// retains its query pool along with the buffers and their native dependencies.
struct GpuTimer {
    owner: Arc<MemoryOwner>,
    pool: vk::QueryPool,
    valid_bits: u32,
    period_ns: f64,
}

impl GpuTimer {
    fn new(owner: Arc<MemoryOwner>) -> Option<Self> {
        let device = &owner.device;
        // SAFETY: The retained Device owns this live Instance/physical device;
        // both queries return owned data and do not access submitted work.
        let families = unsafe {
            device
                .instance_raw()
                .get_physical_device_queue_family_properties(device.physical_device())
        };
        let valid_bits = families[device.queue_family() as usize].timestamp_valid_bits;
        if valid_bits == 0 {
            return None;
        }
        assert!(valid_bits <= 64, "recognized native timestamp width");
        // SAFETY: The same physical device remains live through the owner.
        let properties = unsafe {
            device
                .instance_raw()
                .get_physical_device_properties(device.physical_device())
        };
        let period_ns = f64::from(properties.limits.timestamp_period);
        assert!(period_ns.is_finite() && period_ns > 0.0);
        // SAFETY: This creates two timestamp queries on the live Device. The
        // unique timer immediately owns the successful pool and retains Device.
        let pool = unsafe {
            device.raw().create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(2),
                None,
            )
        }
        .expect("create benchmark timestamp query pool");
        Some(Self {
            owner,
            pool,
            valid_bits,
            period_ns,
        })
    }

    /// # Safety
    /// `command` records on this Device and exclusively retains this timer
    /// through execution. A previous use, if any, has completed on its Fence.
    unsafe fn begin(&self, command: vk::CommandBuffer) {
        // SAFETY: The caller provides exclusive, retained recording ownership;
        // these queries are reset before either timestamp is written.
        unsafe {
            self.owner
                .device
                .raw()
                .cmd_reset_query_pool(command, self.pool, 0, 2);
            self.owner.device.raw().cmd_write_timestamp2(
                command,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                self.pool,
                0,
            );
        }
    }

    /// # Safety
    /// The caller recorded begin and the measured payload on the same command,
    /// and retains this timer until the associated submission retires.
    unsafe fn end(&self, command: vk::CommandBuffer) {
        // SAFETY: The matching begin reset both queries. BOTTOM_OF_PIPE places
        // this timestamp after the measured commands on the supported queue.
        unsafe {
            self.owner.device.raw().cmd_write_timestamp2(
                command,
                vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
                self.pool,
                1,
            );
        }
    }

    /// # Safety
    /// The submission containing both timestamp writes completed on its actual
    /// Fence. No next recording or GPU access overlaps this host result read.
    unsafe fn elapsed_ns(&self) -> f64 {
        let mut ticks = [0_u64; 2];
        // SAFETY: Both reset/write commands completed. The two u64 results have
        // the required alignment/stride; no WAIT flag adds another GPU wait.
        unsafe {
            self.owner.device.raw().get_query_pool_results(
                self.pool,
                0,
                &mut ticks,
                vk::QueryResultFlags::TYPE_64,
            )
        }
        .expect("completed timestamp queries must be available");
        let mask = u64::MAX >> (64 - self.valid_bits);
        (ticks[1].wrapping_sub(ticks[0]) & mask) as f64 * self.period_ns
    }
}

impl Drop for GpuTimer {
    fn drop(&mut self) {
        // SAFETY: This timer is owned by its payload. The transfer owner only
        // releases it before submit or after proven retirement; unresolved
        // submissions retain the entire payload instead of running this Drop.
        unsafe { self.owner.device.raw().destroy_query_pool(self.pool, None) };
    }
}

struct MeasuredTransfer {
    buffers: TransferBuffers,
    timer: Option<GpuTimer>,
}

impl MeasuredTransfer {
    fn new(allocator: &VulkanAllocator) -> Self {
        Self {
            buffers: TransferBuffers {
                upload: allocator
                    .create_buffer(BufferRole::Upload, BYTES as u64)
                    .unwrap(),
                transfer: allocator
                    .create_buffer(BufferRole::DeviceTransfer, BYTES as u64)
                    .unwrap(),
                readback: allocator
                    .create_buffer(BufferRole::Readback, BYTES as u64)
                    .unwrap(),
            },
            timer: GpuTimer::new(Arc::clone(&allocator.owner)),
        }
    }
}

impl TransferPayload for MeasuredTransfer {
    fn validate(&self) -> Result<(), TransferError> {
        self.buffers.validate()
    }

    fn owner(&self) -> &Arc<MemoryOwner> {
        self.buffers.owner()
    }

    fn bytes(&self) -> u64 {
        self.buffers.bytes()
    }

    fn label(&self) -> &'static CStr {
        c"renderer measured upload -> device -> readback"
    }

    unsafe fn record(&self, command: vk::CommandBuffer) {
        // SAFETY: The caller retains this whole payload through recording and
        // completion. Its unique timer and buffers share the same live Device.
        unsafe {
            if let Some(timer) = &self.timer {
                timer.begin(command);
            }
            self.buffers.record(command);
            if let Some(timer) = &self.timer {
                timer.end(command);
            }
        }
    }

    unsafe fn establish_contents(&mut self) {
        // SAFETY: The caller proved Fence completion and an active Device for
        // this payload, including both full-buffer copies and timestamp writes.
        unsafe { self.buffers.establish_contents() };
    }
}

#[derive(Clone, Copy)]
struct Sample {
    write: Duration,
    record: Duration,
    submit: Duration,
    wait: Duration,
    readback: Duration,
    gpu_ns: Option<f64>,
}

fn iteration(
    mut work: MeasuredTransfer,
    pool: Option<&ExecutionPool>,
    pattern: &[u8],
    output: &mut [u8],
) -> (MeasuredTransfer, Sample) {
    let started = Instant::now();
    work.buffers.upload.write(0, pattern).unwrap();
    let write = started.elapsed();
    let started = Instant::now();
    let recorded = match pool {
        Some(pool) => pool.record(work),
        None => RecordedTransfer::new(work),
    }
    .unwrap();
    let record = started.elapsed();
    let started = Instant::now();
    let mut pending = recorded.submit().unwrap();
    let submit = started.elapsed();
    let started = Instant::now();
    let mut completed = pending
        .wait(Duration::from_secs(10))
        .unwrap()
        .expect("benchmark transfer must complete within the test wait budget");
    let wait = started.elapsed();
    let started = Instant::now();
    completed.buffers.readback.read(0, output).unwrap();
    let readback = started.elapsed();
    // Full verification and query retrieval are intentionally outside every
    // CPU timing interval. Fence success precedes query reads, never vice versa.
    assert_eq!(output, pattern, "benchmark readback must match every byte");
    let gpu_ns = completed.timer.as_ref().map(|timer| {
        // SAFETY: The actual wait above completed both timestamps. This payload
        // is exclusively on the CPU and has not been recorded for its next use.
        unsafe { timer.elapsed_ns() }
    });
    (
        completed,
        Sample {
            write,
            record,
            submit,
            wait,
            readback,
            gpu_ns,
        },
    )
}

fn distribution(case: &str, phase: &str, values: impl Iterator<Item = f64>) {
    let mut values: Vec<_> = values.collect();
    values.sort_by(f64::total_cmp);
    let percentile = |percent: usize| values[(values.len() * percent).div_ceil(100) - 1];
    println!(
        "performance case={case} phase={phase} samples={} median_us={:.3} p95_us={:.3}",
        values.len(),
        percentile(50) / 1000.0,
        percentile(95) / 1000.0,
    );
}

fn report(case: &str, samples: &[Sample]) {
    for (phase, select) in [
        ("write", (|s: &Sample| s.write) as fn(&Sample) -> Duration),
        ("record", |s| s.record),
        ("submit", |s| s.submit),
        ("wait_and_retire", |s| s.wait),
        ("readback", |s| s.readback),
        ("cpu_phase_sum", |s| {
            s.write + s.record + s.submit + s.wait + s.readback
        }),
    ] {
        distribution(
            case,
            phase,
            samples.iter().map(|s| select(s).as_nanos() as f64),
        );
    }
    if samples[0].gpu_ns.is_some() {
        distribution(
            case,
            "gpu_commands",
            samples.iter().map(|s| s.gpu_ns.unwrap()),
        );
    } else {
        println!("performance case={case} phase=gpu_commands unavailable=queue_has_no_timestamps");
    }
}

const BATCH_ITEMS: usize = 8;
const BATCH_ITEM_BYTES: usize = 64 * 1024;

fn batch_iteration(
    pool: &TransferPool,
    targets: Vec<VulkanBuffer>,
    patterns: &[Vec<u8>],
    outputs: &mut [Vec<u8>],
    batched: bool,
) -> (Vec<VulkanBuffer>, Duration) {
    let started = Instant::now();
    let mut returned = Vec::with_capacity(BATCH_ITEMS);
    if batched {
        let mut builder = pool.begin().unwrap();
        let mut results = Vec::with_capacity(BATCH_ITEMS);
        for (index, target) in targets.into_iter().enumerate() {
            let key = builder.add_buffer(target).unwrap();
            builder.upload_buffer(key, 0, &patterns[index]).unwrap();
            let readback = builder
                .readback_buffer(key, 0, BATCH_ITEM_BYTES as u64)
                .unwrap();
            results.push((key, readback));
        }
        let mut pending = builder.record().unwrap().submit().unwrap();
        let mut completed = pending
            .wait(Duration::from_secs(10))
            .unwrap()
            .expect("benchmark batch must complete within the wait budget");
        for (index, (key, readback)) in results.into_iter().enumerate() {
            completed.readback(readback, &mut outputs[index]).unwrap();
            returned.push(completed.take_buffer(key).unwrap());
        }
    } else {
        let mut pending = Vec::with_capacity(BATCH_ITEMS);
        for (index, target) in targets.into_iter().enumerate() {
            let mut builder = pool.begin().unwrap();
            let key = builder.add_buffer(target).unwrap();
            builder.upload_buffer(key, 0, &patterns[index]).unwrap();
            let readback = builder
                .readback_buffer(key, 0, BATCH_ITEM_BYTES as u64)
                .unwrap();
            pending.push((builder.record().unwrap().submit().unwrap(), key, readback));
        }
        // Submit every independent request before waiting, so the reference
        // does not artificially serialize CPU/GPU work one item at a time.
        for (index, (mut transfer, key, readback)) in pending.into_iter().enumerate() {
            let mut completed = transfer
                .wait(Duration::from_secs(10))
                .unwrap()
                .expect("benchmark request must complete within the wait budget");
            completed.readback(readback, &mut outputs[index]).unwrap();
            returned.push(completed.take_buffer(key).unwrap());
        }
    }
    let elapsed = started.elapsed();
    // Generating patterns and checking every returned byte are not part of the
    // CPU workload measurement; readback copies and lease retirement are.
    for (output, pattern) in outputs.iter().zip(patterns) {
        assert_eq!(output, pattern, "batch benchmark must match every byte");
    }
    (returned, elapsed)
}

fn batch_comparison(allocator: &VulkanAllocator) {
    let device = &allocator.owner.device;
    // SAFETY: The allocator retains the live Instance and its physical device.
    let properties = unsafe {
        device
            .instance_raw()
            .get_physical_device_properties(device.physical_device())
    };
    let alignment = properties.limits.non_coherent_atom_size.max(4);
    let stride = (BATCH_ITEM_BYTES as u64).div_ceil(alignment) * alignment;
    let capacity = stride.checked_mul(BATCH_ITEMS as u64).unwrap();
    let separate = TransferPool::new(
        Arc::clone(&allocator.owner),
        BATCH_ITEMS,
        capacity,
        capacity,
    )
    .unwrap();
    let batched = TransferPool::new(
        Arc::clone(&allocator.owner),
        BATCH_ITEMS,
        capacity,
        capacity,
    )
    .unwrap();
    let targets = || {
        (0..BATCH_ITEMS)
            .map(|_| {
                allocator
                    .create_buffer(BufferRole::DeviceTransfer, BATCH_ITEM_BYTES as u64)
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    let mut separate_targets = targets();
    let mut batch_targets = targets();
    let mut patterns = vec![vec![0_u8; BATCH_ITEM_BYTES]; BATCH_ITEMS];
    let mut outputs = vec![vec![0_u8; BATCH_ITEM_BYTES]; BATCH_ITEMS];
    let mut separate_samples = Vec::with_capacity(SAMPLES);
    let mut batch_samples = Vec::with_capacity(SAMPLES);
    for pass in 0..WARMUP + SAMPLES {
        for (item, pattern) in patterns.iter_mut().enumerate() {
            for (index, byte) in pattern.iter_mut().enumerate() {
                *byte = index
                    .wrapping_mul(37)
                    .wrapping_add(item * 71)
                    .wrapping_add(pass * 17) as u8;
            }
        }
        let (separate_sample, batch_sample);
        if pass % 2 == 0 {
            (separate_targets, separate_sample) =
                batch_iteration(&separate, separate_targets, &patterns, &mut outputs, false);
            (batch_targets, batch_sample) =
                batch_iteration(&batched, batch_targets, &patterns, &mut outputs, true);
        } else {
            (batch_targets, batch_sample) =
                batch_iteration(&batched, batch_targets, &patterns, &mut outputs, true);
            (separate_targets, separate_sample) =
                batch_iteration(&separate, separate_targets, &patterns, &mut outputs, false);
        }
        if pass >= WARMUP {
            separate_samples.push(separate_sample);
            batch_samples.push(batch_sample);
        }
    }
    println!(
        "performance section=batch_comparison items={} bytes_per_item={} copy_commands_per_iteration={} warmup_per_case={} samples_per_case={} order=alternating gpu_time=not_measured",
        BATCH_ITEMS,
        BATCH_ITEM_BYTES,
        2 * BATCH_ITEMS,
        WARMUP,
        SAMPLES,
    );
    for (name, pool, samples, submissions) in [
        (
            "separate_submissions",
            &separate,
            &separate_samples,
            BATCH_ITEMS,
        ),
        ("single_batch", &batched, &batch_samples, 1),
    ] {
        distribution(
            name,
            "cpu_batch_end_to_end",
            samples.iter().map(|duration| duration.as_nanos() as f64),
        );
        let execution = pool.execution_stats();
        let (upload, readback) = pool.staging_stats();
        assert_eq!(upload.buffer_creates, 1);
        assert_eq!(readback.buffer_creates, 1);
        assert_eq!(upload.live_ranges, 0);
        assert_eq!(readback.live_ranges, 0);
        assert_eq!(execution.leased, 0);
        println!(
            "performance case={name} counters_including_warmup submissions={} copy_commands={} gpu_copy_bytes={} execution_native_creates={} execution_reuses={} execution_resets={} staging_buffer_creates={} upload_high_water_reserved_bytes={} readback_high_water_reserved_bytes={} upload_capacity={} readback_capacity={}",
            submissions * (WARMUP + SAMPLES),
            2 * BATCH_ITEMS * (WARMUP + SAMPLES),
            2 * BATCH_ITEMS * BATCH_ITEM_BYTES * (WARMUP + SAMPLES),
            execution.native_creates,
            execution.reuses,
            execution.resets,
            upload.buffer_creates + readback.buffer_creates,
            upload.high_water_reserved_bytes,
            readback.high_water_reserved_bytes,
            upload.capacity,
            readback.capacity,
        );
    }
    println!(
        "performance batch_all_bytes_matched={} target_buffers_reused=true validation_coverage=false",
        2 * BATCH_ITEMS * (WARMUP + SAMPLES),
    );
}

#[test]
#[ignore = "explicit release performance measurement; set RENDERER_RUN_PERFORMANCE=1; no validation coverage"]
fn performance_transfer_measurements() {
    if cfg!(debug_assertions) {
        panic!("run this measurement with cargo test --release");
    }
    assert_eq!(
        std::env::var("RENDERER_RUN_PERFORMANCE").as_deref(),
        Ok("1"),
        "explicitly opt in with RENDERER_RUN_PERFORMANCE=1"
    );
    // Keep application/Validation logging out of the measured path regardless
    // of the caller's RUST_LOG, while still collecting diagnostics independently.
    let dispatch = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    tracing::dispatcher::with_default(&dispatch, || {
        let diagnostics = Diagnostics::new();
        let instance =
            VulkanInstance::with_validation(diagnostics.clone(), ValidationMode::Disabled)
                .expect("explicit performance Instance");
        // Catch ordinary loader injection, but do not treat banner absence as
        // proof against all overrides: external settings can suppress INFO.
        // Record the environment/settings check alongside measured results.
        assert!(
            !diagnostics.snapshot().messages.iter().any(|message| {
                message.message_id_name.as_deref() == Some("WARNING-CreateInstance-status-message")
            }),
            "Validation was externally injected; remove loader/layer overrides before measuring"
        );
        let device = VulkanDevice::new(instance).unwrap();
        assert_ne!(
            device.device_type(),
            vk::PhysicalDeviceType::CPU,
            "CPU fallback is not a hardware performance baseline"
        );
        println!(
            "performance gpu={:?} os={} arch={} validation_requested=false bytes={} copies_per_submit=2 warmup_per_case={} samples_per_case={} order=alternating",
            device.name(),
            std::env::consts::OS,
            std::env::consts::ARCH,
            BYTES,
            WARMUP,
            SAMPLES,
        );
        let allocator = VulkanAllocator::new(device).unwrap();
        let pool = ExecutionPool::new(Arc::clone(&allocator.owner), 1).unwrap();
        let mut reference = MeasuredTransfer::new(&allocator);
        let mut pooled = MeasuredTransfer::new(&allocator);
        if let Some(timer) = &reference.timer {
            println!(
                "performance timestamp_valid_bits={} timestamp_period_ns={}",
                timer.valid_bits, timer.period_ns
            );
        }
        let mut reference_samples = Vec::with_capacity(SAMPLES);
        let mut pooled_samples = Vec::with_capacity(SAMPLES);
        let mut pattern = vec![0_u8; BYTES];
        let mut output = vec![0_u8; BYTES];
        for pass in 0..WARMUP + SAMPLES {
            for (index, byte) in pattern.iter_mut().enumerate() {
                *byte = index
                    .wrapping_mul(37)
                    .wrapping_add(index / 257)
                    .wrapping_add(pass * 17) as u8;
            }
            let (reference_sample, pooled_sample);
            if pass % 2 == 0 {
                (reference, reference_sample) = iteration(reference, None, &pattern, &mut output);
                (pooled, pooled_sample) = iteration(pooled, Some(&pool), &pattern, &mut output);
            } else {
                (pooled, pooled_sample) = iteration(pooled, Some(&pool), &pattern, &mut output);
                (reference, reference_sample) = iteration(reference, None, &pattern, &mut output);
            }
            if pass >= WARMUP {
                reference_samples.push(reference_sample);
                pooled_samples.push(pooled_sample);
            }
        }
        report("unpooled_reference", &reference_samples);
        report("pooled", &pooled_samples);
        let stats = pool.stats();
        assert_eq!(stats.native_creates, 1);
        assert_eq!(stats.reuses, WARMUP + SAMPLES - 1);
        assert_eq!(stats.resets, WARMUP + SAMPLES);
        assert_eq!(stats.leased, 0);
        println!(
            "performance counters_including_warmup reference_record_new_calls={} pooled_native_creates={} pooled_reuses={} pooled_resets={} submissions_per_case={} payload_bytes_per_case={} gpu_copy_bytes_per_case={}",
            WARMUP + SAMPLES,
            stats.native_creates,
            stats.reuses,
            stats.resets,
            WARMUP + SAMPLES,
            BYTES * (WARMUP + SAMPLES),
            2 * BYTES * (WARMUP + SAMPLES),
        );
        drop(reference);
        drop(pooled);
        drop(pool);
        batch_comparison(&allocator);
        drop(allocator);
        let report = diagnostics.snapshot();
        assert!(
            report.is_clean(),
            "performance run diagnostics including shutdown: {report:#?}"
        );
        println!(
            "performance native_all_bytes_matched={} validation_coverage=false warnings={} errors={} callback_failures={}",
            2 * (WARMUP + SAMPLES),
            report.warnings,
            report.errors,
            report.callback_failures
        );
    });
}

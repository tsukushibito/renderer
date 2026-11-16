//! Changing-input, consumer-owned Compute full-lifecycle memory probe.

use super::compute_fixture::{self as fixture, Expected};
use super::frame_memory::{Report, counts, features, phase, semantics};
#[cfg(feature = "vulkan")]
use renderer::graph::BufferDesc;
use renderer::graph::{CompiledGraph, Graph, GraphPool, PassHandle};
use renderer::measurement::{NativeEvent, RustEvent, Snapshot};
use std::time::Duration;

const IN_FLIGHT: usize = 3;
const GRAPH_SLOTS: usize = IN_FLIGHT * 4;
const WARMUP: usize = 24;
const FRAMES: usize = 24;
const WAIT: Duration = Duration::from_secs(30);

// Measurement-only adapters over each existing public API, not a new RHI.
trait Backend {
    const SCENARIO: &'static str;
    const RECORDING_CANCEL: bool;
    type Pending;
    type Completed;
    fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending;
    fn cancel(&mut self, graph: &CompiledGraph);
    fn complete(pending: &mut Self::Pending) -> Self::Completed;
    fn readback(result: &mut Self::Completed, pass: PassHandle, output: &mut [u8]);
    fn collect(&mut self);
}

fn consume<B: Backend>(mut result: B::Completed, expected: Expected) {
    let mut output = [0; fixture::IMAGE_BYTES];
    for entry in expected.readbacks {
        let bytes = &mut output[..entry.len];
        B::readback(&mut result, entry.pass, bytes);
        assert_eq!(bytes, &entry.bytes[..entry.len], "{}", entry.name);
    }
    drop(result);
}

fn frames<B: Backend>(runtime: &mut B, pool: &mut GraphPool, count: usize) -> Report<5> {
    let mut pending: [Option<(B::Pending, Expected)>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut held: [Option<(B::Completed, Expected)>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut phases = [Snapshot::default(); 5];
    let before = Snapshot::now();
    for frame in 0..count {
        let slot = frame % IN_FLIGHT;
        phase(&mut phases[4], || {
            if let Some((result, expected)) = held[slot].take() {
                consume::<B>(result, expected);
            }
        });
        phase(&mut phases[3], || {
            if let Some((mut work, expected)) = pending[slot].take() {
                held[slot] = Some((B::complete(&mut work), expected));
                drop(work);
            }
        });
        let (graph, expected) = phase(&mut phases[0], || fixture::build(pool, frame));
        let plan = phase(&mut phases[1], || graph.compile().unwrap());
        let work = phase(&mut phases[2], || {
            runtime.cancel(&plan);
            runtime.submit(&plan)
        });
        pending[slot] = Some((work, expected));
        phase(&mut phases[4], || drop(plan));
    }
    for slot in 0..IN_FLIGHT {
        phase(&mut phases[4], || {
            if let Some((result, expected)) = held[slot].take() {
                consume::<B>(result, expected);
            }
        });
        phase(&mut phases[3], || {
            if let Some((mut work, expected)) = pending[slot].take() {
                held[slot] = Some((B::complete(&mut work), expected));
                drop(work);
            }
        });
    }
    phase(&mut phases[4], || {
        for (result, expected) in held.into_iter().flatten() {
            consume::<B>(result, expected);
        }
        drop(pending);
        runtime.collect();
        // Reset all backing slots inside measurement, including retained uploads.
        let empty: [Graph; GRAPH_SLOTS] = std::array::from_fn(|_| pool.begin().unwrap());
        drop(empty);
        assert_eq!(pool.leased_slots(), 0);
    });
    Report {
        total: Snapshot::now().since(before),
        phases,
    }
}

fn controls(startup: Snapshot, measured: Snapshot, shutdown: Snapshot) {
    assert_ne!(startup.rust[RustEvent::Allocate as usize].calls, 0);
    assert_ne!(shutdown.rust[RustEvent::Deallocate as usize].calls, 0);
    for event in [
        NativeEvent::ObjectCreate,
        NativeEvent::ResourceCreate,
        NativeEvent::MemoryAllocate,
        NativeEvent::DescriptorAllocate,
    ] {
        assert_ne!(
            startup.native[event as usize], 0,
            "native setup hook must be exercised"
        );
    }
    for event in [
        NativeEvent::ObjectDestroy,
        NativeEvent::ResourceDestroy,
        NativeEvent::MemoryFree,
        NativeEvent::DescriptorFree,
    ] {
        assert!(
            [startup, measured, shutdown]
                .into_iter()
                .any(|snapshot| snapshot.native[event as usize] != 0),
            "native destruction hook must be exercised"
        );
    }
}

fn report<B: Backend>(
    baseline: bool,
    startup: Snapshot,
    shutdown: Snapshot,
    measured: Report<5>,
    adapter: serde_json::Value,
    validation: serde_json::Value,
    native_capacity: serde_json::Value,
) {
    let capacity = fixture::capacity();
    let json = serde_json::json!({
        "scenario": B::SCENARIO, "mode": if baseline { "baseline" } else { "require_zero" },
        "features": features(), "adapter": adapter, "validation_after_native_teardown": validation,
        "counter_semantics": semantics(), "warmup_frames": WARMUP, "measured_frames": FRAMES,
        "pending_capacity": IN_FLIGHT, "completed_readbacks_held": IN_FLIGHT,
        "graph_slots": GRAPH_SLOTS, "retained_graph_slots_after_window": 0,
        "graph_capacity": { "resources": capacity.resources, "passes": capacity.passes, "accesses": capacity.accesses, "draws": capacity.draws, "bindings": capacity.bindings, "attachments": capacity.attachments, "dependencies": capacity.dependencies, "initial_ranges": capacity.initial_ranges, "name_bytes": capacity.name_bytes, "upload_bytes": capacity.upload_bytes, "edges": capacity.edges, "segments": capacity.segments, "readers": capacity.readers },
        "extent": [fixture::WIDTH, fixture::HEIGHT], "custom_workgroup_size": [4,4,1], "custom_workgroups": [1,2,1],
        "custom_storage_dispatch_counts": [1,2,3], "total_dispatch_counts": [5,6,7],
        "programs": ["consumer HDR sampled multiply", "consumer HDR storage add", "uniform_readback", "storage_readback", "sampled_float_readback"],
        "bindings": "varying byte0/256 Uniform operands, sampled image/sampler, typed storage image reads/writes, GPU-produced readonly storage offsets and write-only storage output",
        "native_capacity": native_capacity, "recording_cancel_each_frame": B::RECORDING_CANCEL,
        "coverage": "continuous ordinary Graph build/compile/native prepare-record-submit/GPU completion/poll-retire/CPU readback/full result and plan return; 3 in-flight and 3 retained results; all HDR pixels and FP32 buffer bytes independently checked; changing inputs, Uniform offsets, storage dispatch count and ping-pong image bindings; no presentation or asset update",
        "caller": "fixed pending/result arrays and 120-byte stack output; explicit waits on reused ring; reporting outside measurement",
        "unobserved": "driver/OS allocation outside configured hooks; absent Vulkan layer startup in Disabled mode does not disprove external layer injection; no time-performance claim",
        "startup_positive_control": counts(startup), "shutdown_outside_normal_frames": counts(shutdown),
        "total": counts(measured.total), "phases_build_compile_submit_complete_return": measured.phases.map(counts),
    });
    println!("FRAME_MEMORY_REPORT={json}");
    assert!(!measured.total.overflowed);
    if !baseline {
        assert!(
            measured.total.is_allocation_free(),
            "Compute lifecycle allocates or frees: {:?}",
            measured.total
        );
    }
}

#[cfg(feature = "vulkan")]
impl Backend for renderer::VulkanRuntime {
    const SCENARIO: &'static str = "vulkan_owned_compute_full_lifecycle";
    const RECORDING_CANCEL: bool = false;
    type Pending = renderer::Submission;
    type Completed = renderer::SubmissionResult;
    fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending {
        self.submit(graph).unwrap()
    }
    fn cancel(&mut self, _: &CompiledGraph) {}
    fn complete(pending: &mut Self::Pending) -> Self::Completed {
        pending.wait(WAIT).unwrap().unwrap()
    }
    fn readback(result: &mut Self::Completed, pass: PassHandle, output: &mut [u8]) {
        assert_eq!(result.readback_len(pass).unwrap(), output.len());
        result.readback_into(pass, output).unwrap();
    }
    fn collect(&mut self) {
        assert!(self.poll_frame().unwrap().is_none());
        assert_eq!(self.pending_frames(), 0);
    }
}

#[cfg(feature = "vulkan")]
pub fn vulkan(baseline: bool) {
    if cfg!(debug_assertions) {
        panic!("memory acceptance requires --release");
    }
    let start = Snapshot::now();
    let mut runtime = renderer::VulkanRuntime::new(renderer::RuntimeConfig {
        execution_slots: IN_FLIGHT,
        resource_capacity: 64,
        staging_bytes: 16 * 1024,
        staging_ranges: 64,
        max_shader_bindings: 32,
    })
    .unwrap();
    let info = runtime.device_info();
    assert_ne!(info.device_type, "CPU");
    let monitor = runtime.validation_monitor();
    runtime
        .prepare_working_set(
            renderer::FrameCapacity {
                graph: fixture::capacity(),
                work_slots: IN_FLIGHT,
                result_slots: IN_FLIGHT * 2,
                metadata_slots: 16,
                range_snapshots: 64,
                initialized_ranges_per_resource: 16,
                provenance_segments_per_resource: 0,
            },
            &[
                renderer::ResourceClass::Buffer {
                    desc: BufferDesc { size: 512 },
                    count: IN_FLIGHT,
                },
                renderer::ResourceClass::Buffer {
                    desc: BufferDesc { size: 64 },
                    count: IN_FLIGHT * 3,
                },
                renderer::ResourceClass::Image {
                    desc: fixture::hdr(),
                    count: IN_FLIGHT * 4,
                },
            ],
        )
        .unwrap();
    runtime
        .prepare_compute_programs(&fixture::programs())
        .unwrap();
    let mut pool = GraphPool::new(fixture::capacity(), GRAPH_SLOTS).unwrap();
    frames(&mut runtime, &mut pool, WARMUP);
    let startup = Snapshot::now().since(start);
    let measured = frames(&mut runtime, &mut pool, FRAMES);
    assert_eq!(pool.leased_slots(), 0);
    let before_shutdown = Snapshot::now();
    drop((runtime, pool));
    let shutdown = Snapshot::now().since(before_shutdown);
    controls(startup, measured.total, shutdown);
    assert_ne!(
        startup.vma_host[renderer::measurement::HostEvent::Allocate as usize].calls,
        0
    );
    assert_ne!(
        shutdown.vma_host[renderer::measurement::HostEvent::Deallocate as usize].calls,
        0
    );
    let validation = monitor.report();
    assert!(validation.is_clean(), "{validation:#?}");
    if cfg!(feature = "development-validation") {
        assert_eq!(validation.core_validation, Some(true));
        assert_eq!(validation.synchronization_validation, Some(true));
    } else {
        assert_eq!(validation.requested, renderer::ValidationMode::Disabled);
    }
    report::<renderer::VulkanRuntime>(
        baseline,
        startup,
        shutdown,
        measured,
        serde_json::json!({ "name": info.name, "vendor_id": info.vendor_id, "device_id": info.device_id, "device_luid": info.device_luid, "driver_version_raw": info.driver_version_raw, "driver_name": info.driver_name, "driver_info": info.driver_info, "api_version": info.api_version }),
        serde_json::json!({ "requested": format!("{:?}", validation.requested), "core_validation": validation.core_validation, "synchronization_validation": validation.synchronization_validation, "warnings": validation.warnings, "errors": validation.errors, "callback_failures": validation.callback_failures, "dropped_problems": validation.dropped_problems }),
        serde_json::json!({ "execution_slots": IN_FLIGHT, "work_slots": IN_FLIGHT, "result_slots": IN_FLIGHT * 2, "resource_capacity": 64, "staging_bytes": 16384, "staging_ranges": 64, "max_shader_bindings": 32, "metadata_slots": 16, "range_snapshots": 64, "initialized_ranges_per_resource": 16, "provenance_segments_per_resource": 0, "resource_classes": [ { "buffer_bytes": 512, "count": IN_FLIGHT }, { "buffer_bytes": 64, "count": IN_FLIGHT * 3 }, { "rgba16f_extent": [fixture::WIDTH, fixture::HEIGHT], "count": IN_FLIGHT * 4 } ] }),
    );
}

#[cfg(all(feature = "d3d12", windows))]
impl Backend for renderer::D3D12Runtime {
    const SCENARIO: &'static str = "d3d12_owned_compute_full_lifecycle";
    const RECORDING_CANCEL: bool = true;
    type Pending = renderer::D3D12Submission;
    type Completed = renderer::D3D12SubmissionResult;
    fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending {
        self.submit(graph).unwrap()
    }
    fn cancel(&mut self, graph: &CompiledGraph) {
        drop(self.prepare(graph).unwrap());
    }
    fn complete(pending: &mut Self::Pending) -> Self::Completed {
        pending.wait(WAIT).unwrap().unwrap()
    }
    fn readback(result: &mut Self::Completed, pass: PassHandle, output: &mut [u8]) {
        assert_eq!(result.readback_len(pass).unwrap(), output.len());
        result.readback_into(pass, output).unwrap();
    }
    fn collect(&mut self) {
        assert_eq!(self.poll().unwrap(), 0);
    }
}

#[cfg(all(feature = "d3d12", windows))]
pub fn d3d12(baseline: bool) {
    if cfg!(debug_assertions) {
        panic!("memory acceptance requires --release");
    }
    let start = Snapshot::now();
    let mut config = renderer::D3D12RuntimeConfig::default();
    config.capacity.executions = IN_FLIGHT;
    config.capacity.results = IN_FLIGHT * 2;
    config.capacity.resources = 16;
    config.capacity.operations = 32;
    config.capacity.provenance_segments_per_resource = 0;
    config.capacity.buffers = 4;
    config.capacity.buffer_bytes = 512;
    config.capacity.upload_bytes = 16 * 1024;
    config.capacity.readback_bytes = 16 * 1024;
    config.capacity.scratch_bytes = 512;
    let mut runtime = renderer::D3D12Runtime::new(config).unwrap();
    let info = runtime.device_info();
    let monitor = runtime.validation_monitor();
    runtime
        .prepare_graph_working_set(
            &[renderer::D3D12ImageClass {
                desc: fixture::hdr(),
                count: 4,
            }],
            &[],
            &fixture::programs(),
        )
        .unwrap();
    let mut pool = GraphPool::new(fixture::capacity(), GRAPH_SLOTS).unwrap();
    frames(&mut runtime, &mut pool, WARMUP);
    let startup = Snapshot::now().since(start);
    let measured = frames(&mut runtime, &mut pool, FRAMES);
    assert_eq!(pool.leased_slots(), 0);
    let before_shutdown = Snapshot::now();
    runtime.shutdown().unwrap();
    drop(pool);
    let shutdown = Snapshot::now().since(before_shutdown);
    controls(startup, measured.total, shutdown);
    let validation = monitor.snapshot();
    assert!(validation.is_clean(), "{validation:#?}");
    if cfg!(feature = "development-validation") {
        assert!(validation.teardown_checked);
    }
    assert_eq!(
        validation.debug_layer_enabled,
        cfg!(feature = "development-validation")
    );
    assert_eq!(
        validation.gpu_based_validation_enabled,
        cfg!(feature = "development-validation")
    );
    report::<renderer::D3D12Runtime>(
        baseline,
        startup,
        shutdown,
        measured,
        serde_json::json!({ "name": info.name, "vendor_id": info.vendor_id, "device_id": info.device_id, "luid": info.luid, "driver_version": info.driver_version, "shader_model": info.shader_model }),
        serde_json::json!({ "debug_layer_enabled": validation.debug_layer_enabled, "gpu_based_validation_enabled": validation.gpu_based_validation_enabled, "teardown_checked": validation.teardown_checked, "warnings": validation.warnings, "errors": validation.errors, "corruption": validation.corruption, "callback_failures": validation.callback_failures, "dropped_messages": validation.dropped_messages }),
        serde_json::json!({ "executions": IN_FLIGHT, "results": IN_FLIGHT * 2, "resources": 16, "operations": 32, "provenance_segments_per_resource": 0, "buffers": 4, "buffer_bytes": 512, "upload_bytes": 16384, "readback_bytes": 16384, "scratch_bytes": 512, "image_classes_per_execution": [{ "rgba16f_extent": [fixture::WIDTH, fixture::HEIGHT], "count": 4 }] }),
    );
}

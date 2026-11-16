//! Changing-input Compute and standard Raster/Compute lifecycle memory probes.

use super::compute_fixture::{self as fixture, Expected};
use super::frame_memory::{Report, counts, features, phase, semantics};
use super::{custom_raster_fixture, standard_fixture};
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

#[derive(Clone, Copy)]
enum Workload {
    Compute,
    Standard,
    CustomRaster,
}

impl Workload {
    fn capacity(self) -> renderer::graph::GraphCapacity {
        match self {
            Self::Compute => fixture::capacity(),
            Self::Standard => standard_fixture::capacity(),
            Self::CustomRaster => custom_raster_fixture::capacity(),
        }
    }

    fn build(self, pool: &mut GraphPool, frame: usize) -> (Graph, Expected) {
        match self {
            Self::Compute => fixture::build(pool, frame),
            Self::Standard => standard_fixture::build(pool, frame),
            Self::CustomRaster => custom_raster_fixture::build(pool, frame),
        }
    }

    fn provenance_capacity(self) -> usize {
        match self {
            Self::Compute => 0,
            Self::Standard | Self::CustomRaster => 16,
        }
    }

    fn buffer_classes(self) -> &'static [(u64, usize)] {
        match self {
            Self::Compute => &[(512, 1), (64, 3)],
            Self::Standard => &[
                (standard_fixture::BUFFER_SIZES[0], 1),
                (standard_fixture::BUFFER_SIZES[1], 1),
                (standard_fixture::BUFFER_SIZES[2], 1),
                (standard_fixture::BUFFER_SIZES[3], 1),
            ],
            Self::CustomRaster => custom_raster_fixture::buffer_classes(),
        }
    }

    fn image_classes(self) -> Vec<(renderer::graph::ImageDesc, usize)> {
        match self {
            Self::Compute => vec![(fixture::hdr(), 4)],
            Self::Standard => vec![
                (standard_fixture::hdr(), standard_fixture::HDR_IMAGES),
                (
                    renderer::graph::ImageDesc::depth32(
                        standard_fixture::WIDTH,
                        standard_fixture::HEIGHT,
                    ),
                    standard_fixture::DEPTH_IMAGES,
                ),
            ],
            Self::CustomRaster => custom_raster_fixture::image_classes(),
        }
    }
}

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

fn frames<B: Backend>(
    runtime: &mut B,
    pool: &mut GraphPool,
    count: usize,
    workload: Workload,
) -> Report<5> {
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
        let (graph, expected) = phase(&mut phases[0], || workload.build(pool, frame));
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
    (baseline, workload): (bool, Workload),
    startup: Snapshot,
    shutdown: Snapshot,
    measured: Report<5>,
    adapter: serde_json::Value,
    validation: serde_json::Value,
    native_capacity: serde_json::Value,
) {
    let capacity = workload.capacity();
    let mut json = serde_json::json!({
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
        "caller": "fixed pending/result arrays and 168-byte stack output; explicit waits on reused ring; reporting outside measurement",
        "readback_bytes_per_frame": fixture::READBACK_BYTES,
        "unobserved": "driver/OS allocation outside configured hooks; absent Vulkan layer startup in Disabled mode does not disprove external layer injection; no time-performance claim",
        "startup_positive_control": counts(startup), "shutdown_outside_normal_frames": counts(shutdown),
        "total": counts(measured.total), "phases_build_compile_submit_complete_return": measured.phases.map(counts),
    });
    if matches!(workload, Workload::Standard) {
        json["scenario"] = B::SCENARIO
            .replace("owned_compute", "standard_composition")
            .into();
        json["draw_counts"] = serde_json::json!([0, 1, 4, 2]);
        json["routes"] = serde_json::json!([
            "standard forward directly",
            "standard forward with identity post",
            "standard forward with consumer post",
            "standard passes with consumer post and output"
        ]);
        json["post_modes"] = serde_json::json!([
            "disabled",
            "multiply then add",
            "add then multiply",
            "multiply then add then add"
        ]);
        json["programs"] = serde_json::json!([
            "mesh_hdr",
            "depth_only",
            "mesh_hdr_prepassed",
            "consumer HDR sampled multiply",
            "consumer HDR storage add",
            "uniform_readback"
        ]);
        json["readback_bytes_per_frame"] = standard_fixture::READBACK_BYTES.into();
        json["coverage"] = "continuous mixed Raster/consumer Compute Graph build/compile/native prepare-record-submit/GPU completion/poll-retire/CPU readback/full result and plan return; all four HDR outputs independently checked; varying draw count, parameters and post modes across all public composition routes; no presentation or asset update".into();
        json["bindings"] = "immutable per-frame Raster parameters, sampled HDR/sampler/Uniform, typed storage HDR and GPU-produced readonly storage offsets".into();
        json.as_object_mut()
            .unwrap()
            .remove("custom_storage_dispatch_counts");
        json.as_object_mut()
            .unwrap()
            .remove("total_dispatch_counts");
    }
    if matches!(workload, Workload::CustomRaster) {
        json["scenario"] = B::SCENARIO.replace("owned_compute", "custom_raster").into();
        json["routes"] = serde_json::json!([
            "standard forward directly",
            "standard post replaced by consumer Raster",
            "standard opaque plus consumer Raster plus standard output"
        ]);
        json["programs"] = serde_json::json!([
            "mesh_hdr",
            "consumer straight alpha",
            "consumer premultiplied alpha",
            "consumer additive",
            "consumer replace"
        ]);
        json["bindings"] = "24-byte vertex stride; 32-byte VS/FS inline words; sparse Uniform/StorageRead/sampled image/sampler slots across three groups".into();
        json["readback_bytes_per_frame"] = custom_raster_fixture::READBACK_BYTES.into();
        json["coverage"] = "continuous Graph build/compile/native prepare-record-submit/GPU completion/poll-retire/CPU readback/full result and plan return; changing draw counts, constants, descriptor offsets, blend state and draw order; HDR content and unchanged read-only depth independently checked; no presentation or asset update".into();
        for key in [
            "custom_workgroup_size",
            "custom_workgroups",
            "custom_storage_dispatch_counts",
            "total_dispatch_counts",
        ] {
            json.as_object_mut().unwrap().remove(key);
        }
    }
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
    vulkan_workload(baseline, Workload::Compute);
}

#[cfg(feature = "vulkan")]
pub fn vulkan_standard(baseline: bool) {
    vulkan_workload(baseline, Workload::Standard);
}

#[cfg(feature = "vulkan")]
pub fn vulkan_custom_raster(baseline: bool) {
    vulkan_workload(baseline, Workload::CustomRaster);
}

#[cfg(feature = "vulkan")]
fn vulkan_workload(baseline: bool, workload: Workload) {
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
                graph: workload.capacity(),
                work_slots: IN_FLIGHT,
                result_slots: IN_FLIGHT * 2,
                metadata_slots: 16,
                range_snapshots: 64,
                initialized_ranges_per_resource: 16,
                provenance_segments_per_resource: workload.provenance_capacity(),
            },
            &workload
                .buffer_classes()
                .iter()
                .map(|&(size, count)| renderer::ResourceClass::Buffer {
                    desc: BufferDesc { size },
                    count: count * IN_FLIGHT,
                })
                .chain(workload.image_classes().into_iter().map(|(desc, count)| {
                    renderer::ResourceClass::Image {
                        desc,
                        count: count * IN_FLIGHT,
                    }
                }))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    match workload {
        Workload::Compute => {
            runtime
                .prepare_compute_programs(&fixture::programs())
                .unwrap();
        }
        Workload::Standard => {
            runtime
                .prepare_compute_programs(&standard_fixture::programs())
                .unwrap();
            runtime
                .prepare_raster_programs(&standard_fixture::raster_programs())
                .unwrap();
        }
        Workload::CustomRaster => runtime
            .prepare_raster_programs(&custom_raster_fixture::raster_programs())
            .unwrap(),
    }
    let mut pool = GraphPool::new(workload.capacity(), GRAPH_SLOTS).unwrap();
    frames(&mut runtime, &mut pool, WARMUP, workload);
    let startup = Snapshot::now().since(start);
    let measured = frames(&mut runtime, &mut pool, FRAMES, workload);
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
        (baseline, workload),
        startup,
        shutdown,
        measured,
        serde_json::json!({ "name": info.name, "vendor_id": info.vendor_id, "device_id": info.device_id, "device_luid": info.device_luid, "driver_version_raw": info.driver_version_raw, "driver_name": info.driver_name, "driver_info": info.driver_info, "api_version": info.api_version }),
        serde_json::json!({ "requested": format!("{:?}", validation.requested), "core_validation": validation.core_validation, "synchronization_validation": validation.synchronization_validation, "warnings": validation.warnings, "errors": validation.errors, "callback_failures": validation.callback_failures, "dropped_problems": validation.dropped_problems }),
        serde_json::json!({ "execution_slots": IN_FLIGHT, "work_slots": IN_FLIGHT, "result_slots": IN_FLIGHT * 2, "resource_capacity": 64, "staging_bytes": 16384, "staging_ranges": 64, "max_shader_bindings": 32, "metadata_slots": 16, "range_snapshots": 64, "initialized_ranges_per_resource": 16, "provenance_segments_per_resource": workload.provenance_capacity(), "buffer_classes": workload.buffer_classes().iter().map(|&(size,count)| serde_json::json!({ "buffer_bytes": size, "count": count * IN_FLIGHT })).collect::<Vec<_>>(), "image_classes": workload.image_classes().into_iter().map(|(desc,count)| serde_json::json!({ "format": format!("{:?}", desc.format), "extent": [desc.width,desc.height], "count": count * IN_FLIGHT })).collect::<Vec<_>>() }),
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
    d3d12_workload(baseline, Workload::Compute);
}

#[cfg(all(feature = "d3d12", windows))]
pub fn d3d12_standard(baseline: bool) {
    d3d12_workload(baseline, Workload::Standard);
}

#[cfg(all(feature = "d3d12", windows))]
pub fn d3d12_custom_raster(baseline: bool) {
    d3d12_workload(baseline, Workload::CustomRaster);
}

#[cfg(all(feature = "d3d12", windows))]
fn d3d12_workload(baseline: bool, workload: Workload) {
    if cfg!(debug_assertions) {
        panic!("memory acceptance requires --release");
    }
    let start = Snapshot::now();
    let mut config = renderer::D3D12RuntimeConfig::default();
    config.capacity.executions = IN_FLIGHT;
    config.capacity.results = IN_FLIGHT * 2;
    config.capacity.resources = workload.capacity().resources;
    config.capacity.operations = 32;
    config.capacity.provenance_segments_per_resource = workload.provenance_capacity();
    let buffers = workload
        .buffer_classes()
        .iter()
        .map(|&(_, count)| count)
        .sum();
    config.capacity.buffers = buffers;
    config.capacity.buffer_bytes = 512;
    config.capacity.upload_bytes = 16 * 1024;
    config.capacity.readback_bytes = 16 * 1024;
    config.capacity.scratch_bytes = 512;
    let mut runtime = renderer::D3D12Runtime::new(config).unwrap();
    let info = runtime.device_info();
    let monitor = runtime.validation_monitor();
    let image_classes = workload
        .image_classes()
        .into_iter()
        .map(|(desc, count)| renderer::D3D12ImageClass { desc, count })
        .collect::<Vec<_>>();
    match workload {
        Workload::Compute => runtime
            .prepare_graph_working_set(&image_classes, &[], &fixture::programs())
            .unwrap(),
        Workload::Standard => runtime
            .prepare_graph_working_set(
                &image_classes,
                &standard_fixture::raster_programs(),
                &standard_fixture::programs(),
            )
            .unwrap(),
        Workload::CustomRaster => runtime
            .prepare_graph_working_set(
                &image_classes,
                &custom_raster_fixture::raster_programs(),
                &[],
            )
            .unwrap(),
    }
    drop(image_classes);
    let mut pool = GraphPool::new(workload.capacity(), GRAPH_SLOTS).unwrap();
    frames(&mut runtime, &mut pool, WARMUP, workload);
    let startup = Snapshot::now().since(start);
    let measured = frames(&mut runtime, &mut pool, FRAMES, workload);
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
        (baseline, workload),
        startup,
        shutdown,
        measured,
        serde_json::json!({ "name": info.name, "vendor_id": info.vendor_id, "device_id": info.device_id, "luid": info.luid, "driver_version": info.driver_version, "shader_model": info.shader_model }),
        serde_json::json!({ "debug_layer_enabled": validation.debug_layer_enabled, "gpu_based_validation_enabled": validation.gpu_based_validation_enabled, "teardown_checked": validation.teardown_checked, "warnings": validation.warnings, "errors": validation.errors, "corruption": validation.corruption, "callback_failures": validation.callback_failures, "dropped_messages": validation.dropped_messages }),
        serde_json::json!({ "executions": IN_FLIGHT, "results": IN_FLIGHT * 2, "resources": workload.capacity().resources, "operations": 32, "provenance_segments_per_resource": workload.provenance_capacity(), "buffers": buffers, "buffer_bytes": 512, "upload_bytes": 16384, "readback_bytes": 16384, "scratch_bytes": 512, "image_classes_per_execution": workload.image_classes().into_iter().map(|(desc,count)| serde_json::json!({ "format": format!("{:?}",desc.format), "extent": [desc.width,desc.height], "count": count })).collect::<Vec<_>>() }),
    );
}

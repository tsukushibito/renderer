//! Fixed-storage measurement driver. Report construction is outside the window.

use renderer::engine::{
    Draw, Engine, FrameGraph, FrameSnapshot, IDENTITY, Material, MeshData, Vertex, View,
};
use renderer::graph::{CompiledGraph, ComputeTransform, ImageDesc};
use renderer::measurement::Snapshot;

const IN_FLIGHT: usize = 3;
const WARMUP: usize = 12;
const FRAMES: usize = 24;

struct Report<const N: usize> {
    total: Snapshot,
    phases: [Snapshot; N],
}

fn add(total: &mut Snapshot, value: Snapshot) {
    for (total, value) in total.rust.iter_mut().zip(value.rust) {
        total.calls += value.calls;
        total.bytes += value.bytes;
    }
    for (total, value) in total.vma_host.iter_mut().zip(value.vma_host) {
        total.calls += value.calls;
        total.bytes += value.bytes;
    }
    for (total, value) in total.native.iter_mut().zip(value.native) {
        *total += value;
    }
    total.overflowed |= value.overflowed;
}

fn phase<T>(totals: &mut Snapshot, operation: impl FnOnce() -> T) -> T {
    let before = Snapshot::now();
    let value = operation();
    add(totals, Snapshot::now().since(before));
    value
}

fn cpu_frames(engine: &Engine, draws: &[Draw; 4], count: usize) -> Report<4> {
    let mut captures: [Option<FrameSnapshot>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut graphs: [Option<FrameGraph>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut plans: [Option<CompiledGraph>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut phases = [Snapshot::default(); 4];
    let before = Snapshot::now();
    for frame in 0..count {
        let slot = frame % IN_FLIGHT;
        phase(&mut phases[3], || {
            drop(plans[slot].take());
            drop(graphs[slot].take());
            drop(captures[slot].take());
        });
        let draw_count = [0, 1, 4, 2][frame % 4];
        let view = View {
            width: 16,
            height: 16,
            clip_from_world: IDENTITY,
            clear_color: [frame as f32 / count as f32, 0.25, 0.5, 1.0],
        };
        let captured = phase(&mut phases[0], || {
            engine.snapshot(&view, &draws[..draw_count]).unwrap()
        });
        let extra_passes = frame % 3;
        let graph = phase(&mut phases[1], || {
            captured
                .build_graph(true, |graph, mut color| {
                    for _ in 0..extra_passes {
                        let output = graph.image("memory transform", ImageDesc::rgba8(16, 16))?;
                        graph.compute(
                            "memory transform",
                            color,
                            output,
                            ComputeTransform::Invert,
                        )?;
                        color = output;
                    }
                    Ok(color)
                })
                .unwrap()
        });
        let plan = phase(&mut phases[2], || graph.graph.compile().unwrap());
        assert_eq!(
            plan.pass_count(),
            if draw_count == 0 { 2 } else { 4 } + extra_passes
        );
        captures[slot] = Some(captured);
        graphs[slot] = Some(graph);
        plans[slot] = Some(plan);
    }
    // Drain every normal-frame owner before stopping: plans/graphs/captures
    // from the last three frames must not be deallocated outside measurement.
    phase(&mut phases[3], || drop((plans, graphs, captures)));
    Report {
        total: Snapshot::now().since(before),
        phases,
    }
}

fn counts(snapshot: Snapshot) -> serde_json::Value {
    serde_json::json!({
        "rust_calls_alloc_alloc_zeroed_realloc_dealloc": snapshot.rust.map(|value| value.calls),
        "rust_requested_bytes": snapshot.rust.map(|value| value.bytes),
        "vma_host_calls_alloc_realloc_free": snapshot.vma_host.map(|value| value.calls),
        "vma_host_requested_bytes": snapshot.vma_host.map(|value| value.bytes),
        "native_object_create_destroy_resource_create_destroy_memory_alloc_free_descriptor_alloc_free_reset": snapshot.native,
        "overflowed": snapshot.overflowed,
    })
}

fn features() -> serde_json::Value {
    serde_json::json!({
        "development_validation": cfg!(feature = "development-validation"),
        "graph_diagnostics": cfg!(feature = "graph-diagnostics"),
        "vulkan_diagnostics": cfg!(feature = "vulkan-diagnostics"),
        "d3d12_diagnostics": cfg!(feature = "d3d12-diagnostics"),
        "vulkan": cfg!(feature = "vulkan"),
        "d3d12": cfg!(feature = "d3d12"),
        "presentation": cfg!(feature = "presentation"),
        "debug_assertions": cfg!(debug_assertions),
    })
}

pub fn cpu_lifecycle(baseline: bool) {
    if cfg!(debug_assertions) {
        panic!("memory acceptance requires --release");
    }
    let mut engine = Engine::new(1, 1).unwrap();
    let mesh = engine
        .create_mesh(
            MeshData::new(
                &[
                    Vertex {
                        position: [-1.0, -1.0, 0.5],
                        color: [1.0; 4],
                    },
                    Vertex {
                        position: [1.0, -1.0, 0.5],
                        color: [1.0; 4],
                    },
                    Vertex {
                        position: [0.0, 1.0, 0.5],
                        color: [1.0; 4],
                    },
                ],
                &[0, 1, 2],
            )
            .unwrap(),
        )
        .unwrap();
    let material = engine.create_material(Material::default()).unwrap();
    let draws = [Draw {
        mesh,
        material,
        world_from_model: IDENTITY,
    }; 4];
    cpu_frames(&engine, &draws, WARMUP);
    let report = cpu_frames(&engine, &draws, FRAMES);
    // Serialization, formatting, and all reporter-owned allocations are outside
    // both total and phase intervals. The Engine resident assets remain alive.
    let json = serde_json::json!({
        "scenario": "cpu_snapshot_build_compile_return",
        "mode": if baseline { "baseline" } else { "require_zero" },
        "features": features(),
        "warmup_frames": WARMUP,
        "measured_frames": FRAMES,
        "retained_frame_slots": IN_FLIGHT,
        "draw_counts": [0, 1, 4, 2],
        "extra_compute_passes": [0, 1, 2],
        "coverage": "Rust snapshot/build/compile and all normal-frame owners dropped; no native execution or presentation",
        "total": counts(report.total),
        "phases_snapshot_build_compile_return": report.phases.map(counts),
    });
    println!("FRAME_MEMORY_REPORT={json}");
    assert!(!report.total.overflowed, "measurement counter overflowed");
    if !baseline {
        assert!(
            report.total.is_allocation_free(),
            "CPU lifecycle still allocates or frees: {:?}",
            report.total
        );
    }
}

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
mod buffer {
    use super::*;
    use renderer::graph::{BufferDesc, Graph, PassHandle};
    use renderer::measurement::NativeEvent;
    use std::sync::Arc;
    use std::time::Duration;

    // A measurement adapter only: both normal public APIs exercise the same
    // graph and ownership schedule. It exposes no renderer-internal operands.
    trait Backend {
        type Pending;
        type Completed;
        fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending;
        fn complete(pending: &mut Self::Pending) -> Self::Completed;
        fn consume(result: &mut Self::Completed, pass: PassHandle, expected: &[u8; 16]);
    }

    type Slot<T> = Option<(T, PassHandle, [u8; 16])>;

    fn graph(input: &Arc<[u8]>, copies: usize) -> (Graph, PassHandle) {
        let mut graph = Graph::new();
        let a = graph
            .buffer("memory source", BufferDesc { size: 16 })
            .unwrap();
        graph
            .upload_buffer_shared("memory upload", a, 0, Arc::clone(input))
            .unwrap();
        let mut output = a;
        if copies != 0 {
            let b = graph
                .buffer("memory copy", BufferDesc { size: 16 })
                .unwrap();
            graph.copy_buffer("memory copy", a, 0, b, 0, 16).unwrap();
            output = b;
            if copies == 2 {
                graph
                    .copy_buffer("memory copy back", b, 0, a, 0, 16)
                    .unwrap();
                output = a;
            }
        }
        let read = graph
            .readback_buffer("memory readback", output, 0, 16)
            .unwrap();
        (graph, read)
    }

    fn frames<B: Backend>(
        runtime: &mut B,
        inputs: &[Arc<[u8]>; 4],
        expected: &[[u8; 16]; 4],
        count: usize,
    ) -> Report<5> {
        let mut pending: [Slot<B::Pending>; IN_FLIGHT] = std::array::from_fn(|_| None);
        let mut held: [Slot<B::Completed>; IN_FLIGHT] = std::array::from_fn(|_| None);
        let mut phases = [Snapshot::default(); 5];
        let start = Snapshot::now();
        for frame in 0..count {
            let slot = frame % IN_FLIGHT;
            // Completed readbacks survive another complete ring, independently
            // of GPU retirement. No result is silently dropped after measurement.
            phase(&mut phases[4], || {
                if let Some((mut result, pass, bytes)) = held[slot].take() {
                    B::consume(&mut result, pass, &bytes);
                    drop(result);
                }
            });
            phase(&mut phases[3], || {
                if let Some((mut work, pass, bytes)) = pending[slot].take() {
                    held[slot] = Some((B::complete(&mut work), pass, bytes));
                    drop(work);
                }
            });
            let version = frame % inputs.len();
            let (graph, pass) = phase(&mut phases[0], || graph(&inputs[version], frame % 3));
            let plan = phase(&mut phases[1], || graph.compile().unwrap());
            assert_eq!(plan.pass_count(), 2 + frame % 3);
            let work = phase(&mut phases[2], || runtime.submit(&plan));
            pending[slot] = Some((work, pass, expected[version]));
            phase(&mut phases[4], || drop((plan, graph)));
        }
        for slot in 0..IN_FLIGHT {
            phase(&mut phases[4], || {
                if let Some((mut result, pass, bytes)) = held[slot].take() {
                    B::consume(&mut result, pass, &bytes);
                    drop(result);
                }
            });
            phase(&mut phases[3], || {
                if let Some((mut work, pass, bytes)) = pending[slot].take() {
                    held[slot] = Some((B::complete(&mut work), pass, bytes));
                    drop(work);
                }
            });
        }
        phase(&mut phases[4], || {
            for (mut result, pass, bytes) in held.into_iter().flatten() {
                B::consume(&mut result, pass, &bytes);
                drop(result);
            }
            drop(pending);
        });
        Report {
            total: Snapshot::now().since(start),
            phases,
        }
    }

    fn inputs() -> ([Arc<[u8]>; 4], [[u8; 16]; 4]) {
        let expected = std::array::from_fn(|version| {
            std::array::from_fn(|byte| ((byte * 37 + version * 19) ^ (byte >> 1)) as u8)
        });
        (expected.map(|bytes| Arc::from(bytes.as_slice())), expected)
    }

    fn positive_control(startup: Snapshot) {
        assert!(
            startup.native[NativeEvent::ObjectCreate as usize] != 0,
            "native object hooks are absent or unexercised"
        );
        assert!(
            startup.native[NativeEvent::ResourceCreate as usize] != 0,
            "native resource hooks are absent or unexercised"
        );
    }

    fn destruction_control(intervals: [Snapshot; 3]) {
        assert!(
            intervals
                .iter()
                .any(|interval| interval.native[NativeEvent::ObjectDestroy as usize] != 0),
            "native object destruction hooks are absent or unexercised"
        );
        assert!(
            intervals
                .iter()
                .any(|interval| interval.native[NativeEvent::ResourceDestroy as usize] != 0),
            "native resource destruction hooks are absent or unexercised"
        );
    }

    fn report(
        name: &str,
        baseline: bool,
        startup: Snapshot,
        shutdown: Snapshot,
        result: Report<5>,
    ) {
        let json = serde_json::json!({
            "scenario": name,
            "mode": if baseline { "baseline" } else { "require_zero" },
            "features": features(),
            "native_request": if cfg!(feature = "development-validation") { "Required" } else { "Disabled" },
            "warmup_frames": WARMUP,
            "measured_frames": FRAMES,
            "pending_capacity_exercised": IN_FLIGHT,
            "completed_readbacks_held": IN_FLIGHT,
            "input_versions": 4,
            "copy_passes": [0, 1, 2],
            "coverage": "public buffer Graph build/compile/submit/complete/readback/return; no raster/image/WSI or cancel/busy proof",
            "caller": "fixed arrays and prepared input Arcs; report outside measurement; explicit per-submission fence waits",
            "unobserved": "driver-internal host/device allocations",
            "startup_positive_control": counts(startup),
            "shutdown_outside_normal_frames": counts(shutdown),
            "total": counts(result.total),
            "phases_build_compile_submit_complete_return": result.phases.map(counts),
        });
        println!("FRAME_MEMORY_REPORT={json}");
        assert!(!result.total.overflowed);
        if !baseline {
            assert!(
                result.total.is_allocation_free(),
                "buffer lifecycle still allocates or frees: {:?}",
                result.total
            );
        }
    }

    #[cfg(feature = "vulkan")]
    impl Backend for renderer::VulkanRuntime {
        type Pending = renderer::Submission;
        type Completed = renderer::SubmissionResult;
        fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending {
            self.submit(graph).unwrap()
        }
        fn complete(pending: &mut Self::Pending) -> Self::Completed {
            pending.wait(Duration::from_secs(30)).unwrap().unwrap()
        }
        fn consume(result: &mut Self::Completed, pass: PassHandle, expected: &[u8; 16]) {
            let bytes = result.readback(pass).unwrap();
            assert_eq!(bytes, expected);
            drop(bytes);
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
            resource_capacity: 32,
            staging_bytes: 16 * 1024,
            max_shader_bindings: 8,
        })
        .unwrap();
        assert_ne!(runtime.device_info().device_type, "CPU");
        let monitor = runtime.validation_monitor();
        let (inputs, expected) = inputs();
        frames(&mut runtime, &inputs, &expected, WARMUP);
        let startup = Snapshot::now().since(start);
        positive_control(startup);
        assert!(
            startup.vma_host[0].calls != 0,
            "VMA host hooks are absent or unexercised"
        );
        assert!(
            startup.native[NativeEvent::MemoryAllocate as usize] != 0,
            "VMA device memory hooks are absent or unexercised"
        );
        let measured = frames(&mut runtime, &inputs, &expected, FRAMES);
        let before_shutdown = Snapshot::now();
        drop(runtime);
        let shutdown = Snapshot::now().since(before_shutdown);
        destruction_control([startup, measured.total, shutdown]);
        assert!(
            shutdown.vma_host[renderer::measurement::HostEvent::Deallocate as usize].calls != 0,
            "VMA host free hooks are absent or unexercised"
        );
        assert!(
            shutdown.native[NativeEvent::MemoryFree as usize] != 0,
            "VMA device memory free hooks are absent or unexercised"
        );
        let validation = monitor.report();
        assert!(validation.is_clean(), "{validation:#?}");
        if cfg!(feature = "development-validation") {
            assert_eq!(validation.core_validation, Some(true));
            assert_eq!(validation.synchronization_validation, Some(true));
        } else {
            assert_eq!(validation.requested, renderer::ValidationMode::Disabled);
        }
        report(
            "vulkan_buffer_full_lifecycle",
            baseline,
            startup,
            shutdown,
            measured,
        );
    }

    #[cfg(all(feature = "d3d12", windows))]
    impl Backend for renderer::D3D12Runtime {
        type Pending = renderer::D3D12Submission;
        type Completed = renderer::D3D12SubmissionResult;
        fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending {
            self.submit(graph).unwrap()
        }
        fn complete(pending: &mut Self::Pending) -> Self::Completed {
            pending.wait(Duration::from_secs(30)).unwrap().unwrap()
        }
        fn consume(result: &mut Self::Completed, pass: PassHandle, expected: &[u8; 16]) {
            let bytes = result.readback(pass).unwrap();
            assert_eq!(bytes, expected);
            drop(bytes);
        }
    }

    #[cfg(all(feature = "d3d12", windows))]
    pub fn d3d12(baseline: bool) {
        if cfg!(debug_assertions) {
            panic!("memory acceptance requires --release");
        }
        let start = Snapshot::now();
        let mut runtime =
            renderer::D3D12Runtime::new(renderer::D3D12RuntimeConfig::default()).unwrap();
        let monitor = runtime.validation_monitor();
        let (inputs, expected) = inputs();
        frames(&mut runtime, &inputs, &expected, WARMUP);
        let startup = Snapshot::now().since(start);
        positive_control(startup);
        let measured = frames(&mut runtime, &inputs, &expected, FRAMES);
        let before_shutdown = Snapshot::now();
        runtime.shutdown().unwrap();
        let shutdown = Snapshot::now().since(before_shutdown);
        destruction_control([startup, measured.total, shutdown]);
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
        report(
            "d3d12_buffer_full_lifecycle",
            baseline,
            startup,
            shutdown,
            measured,
        );
    }
}

#[cfg(all(feature = "d3d12", windows))]
pub use buffer::d3d12 as d3d12_buffer_lifecycle;
#[cfg(feature = "vulkan")]
pub use buffer::vulkan as vulkan_buffer_lifecycle;

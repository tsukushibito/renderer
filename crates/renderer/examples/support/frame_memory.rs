//! Fixed-storage measurement driver. Report construction is outside the window.

use renderer::engine::{
    Draw, Engine, EngineCapacity, EngineErrorKind, FrameSnapshot, IDENTITY, Material, MeshData,
    SnapshotPool, Vertex, View,
};
use renderer::graph::{
    BufferDesc, CompiledGraph, ComputeTransform, Graph, GraphCapacity, GraphErrorKind, GraphPool,
    ImageDesc,
};
use renderer::measurement::Snapshot;

const IN_FLIGHT: usize = 3;
const WARMUP: usize = 12;
const FRAMES: usize = 24;

pub(crate) struct Report<const N: usize> {
    pub(crate) total: Snapshot,
    pub(crate) phases: [Snapshot; N],
}

pub(crate) fn phase<T>(totals: &mut Snapshot, operation: impl FnOnce() -> T) -> T {
    let before = Snapshot::now();
    let value = operation();
    totals.accumulate(Snapshot::now().since(before));
    value
}

pub(crate) fn graph_capacity() -> GraphCapacity {
    GraphCapacity {
        resources: 8,
        passes: 8,
        accesses: 32,
        draws: 4,
        dependencies: 8,
        initial_ranges: 16,
        name_bytes: 512,
        upload_bytes: 128,
        edges: 128,
        segments: 128,
        readers: 128,
    }
}

pub(crate) fn capacity_report() -> serde_json::Value {
    let capacity = graph_capacity();
    serde_json::json!({
        "resources": capacity.resources, "passes": capacity.passes,
        "accesses": capacity.accesses, "draws": capacity.draws,
        "dependencies": capacity.dependencies, "initial_ranges": capacity.initial_ranges,
        "name_bytes": capacity.name_bytes, "upload_bytes": capacity.upload_bytes,
        "edges": capacity.edges, "segments": capacity.segments, "readers": capacity.readers,
    })
}

fn recycle_graph_slots<const N: usize>(pool: &mut GraphPool) -> usize {
    // Reset every idle backing slot while it is uniquely owned. Retain the empty
    // graphs together so begin() cannot repeatedly select just the first slot.
    // Idle slots return upload/import operands inside measurement. Canonical
    // buffer content proofs may still pin other slots as persistent state;
    // report those pins instead of invalidating them just for measurement.
    let mut empty: [Option<Graph>; N] = std::array::from_fn(|_| None);
    let mut count = 0;
    for slot in &mut empty {
        match pool.begin() {
            Ok(graph) => {
                *slot = Some(graph);
                count += 1;
            }
            Err(error) => {
                assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
                break;
            }
        }
    }
    drop(empty);
    count
}

fn cpu_frames(
    engine: &Engine,
    snapshots: &SnapshotPool,
    pool: &mut GraphPool,
    draws: &[Draw; 4],
    count: usize,
) -> Report<4> {
    let mut captures: [Option<FrameSnapshot>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut plans: [Option<CompiledGraph>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut phases = [Snapshot::default(); 4];
    let before = Snapshot::now();
    for frame in 0..count {
        let slot = frame % IN_FLIGHT;
        phase(&mut phases[3], || {
            drop(plans[slot].take());
            drop(captures[slot].take());
        });
        let draw_count = [0, 1, 4, 2][frame % 4];
        let view = View {
            width: 16,
            height: 16,
            clip_from_world: IDENTITY,
            clear_color: [frame as f32 / count as f32, 0.25, 0.5, 1.0],
        };
        phase(&mut phases[0], || {
            let over_capacity = [draws[0]; 5];
            assert_eq!(
                engine
                    .snapshot(snapshots, &view, &over_capacity)
                    .err()
                    .unwrap()
                    .kind(),
                EngineErrorKind::CapacityExhausted
            );
            // Abandon an acquired snapshot without building a frame. Its slot
            // must return without waiting or allocating replacement storage.
            drop(engine.snapshot(snapshots, &view, &[]).unwrap());
        });
        phase(&mut phases[1], || {
            // Fill the prepared resource capacity exactly, reject the next
            // declaration, then cancel this uncompiled graph inside the window.
            let mut canceled = pool.begin().unwrap();
            for _ in 0..graph_capacity().resources {
                canceled
                    .buffer("canceled", BufferDesc { size: 16 })
                    .unwrap();
            }
            assert_eq!(
                canceled
                    .buffer("over capacity", BufferDesc { size: 16 })
                    .err()
                    .unwrap()
                    .kind(),
                GraphErrorKind::CapacityExhausted
            );
            drop(canceled);
        });
        let captured = phase(&mut phases[0], || {
            engine
                .snapshot(snapshots, &view, &draws[..draw_count])
                .unwrap()
        });
        let extra_passes = frame % 3;
        let graph = phase(&mut phases[1], || {
            captured
                .build_graph(pool, true, |graph, mut color| {
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
        plans[slot] = Some(plan);
        if frame >= IN_FLIGHT - 1 {
            phase(&mut phases[0], || {
                assert_eq!(
                    engine.snapshot(snapshots, &view, &[]).err().unwrap().kind(),
                    EngineErrorKind::CapacityExhausted
                );
            });
            phase(&mut phases[1], || {
                assert_eq!(
                    pool.begin().err().unwrap().kind(),
                    GraphErrorKind::CapacityExhausted
                );
            });
        }
    }
    // Drain every normal-frame owner before stopping: plans/captures
    // from the last three frames must not be deallocated outside measurement.
    phase(&mut phases[3], || {
        drop((plans, captures));
        assert_eq!(recycle_graph_slots::<IN_FLIGHT>(pool), IN_FLIGHT);
    });
    Report {
        total: Snapshot::now().since(before),
        phases,
    }
}

pub(crate) fn counts(snapshot: Snapshot) -> serde_json::Value {
    serde_json::json!({
        "rust_calls_alloc_alloc_zeroed_realloc_dealloc": snapshot.rust.map(|value| value.calls),
        "rust_requested_bytes": snapshot.rust.map(|value| value.bytes),
        "vma_host_calls_alloc_realloc_free": snapshot.vma_host.map(|value| value.calls),
        "vma_host_requested_bytes": snapshot.vma_host.map(|value| value.bytes),
        "native_object_create_destroy_resource_create_destroy_memory_alloc_free_descriptor_alloc_free_reset": snapshot.native,
        "overflowed": snapshot.overflowed,
    })
}

pub(crate) fn semantics() -> serde_json::Value {
    serde_json::json!({
        "rust": "allocator entry-point calls and requested bytes, including frees of allocations made before the interval",
        "vma_host": "VMA native host callback calls/known requested bytes, forwarded separately from the Rust global allocator",
        "native_objects_resources": "Renderer factory attempts including failures; final Renderer owner release for destruction",
        "native_memory": "Vulkan successful VMA device-memory notifications; D3D12 committed-resource implicit heap allocation attempts/final owner release",
        "native_descriptors": "requested/freed Vulkan descriptor sets or D3D12 heap descriptor elements, not API calls",
        "native_reset": "reuse/reset events; implicit descriptor frees are also counted separately",
        "scope": "overlapping categories must not be summed as physical allocations; driver-internal allocations are unobserved",
    })
}

pub(crate) fn features() -> serde_json::Value {
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
    let start = Snapshot::now();
    let mut engine = Engine::new(EngineCapacity {
        meshes: 1,
        materials: 1,
        retired_mesh_versions: 1,
    })
    .unwrap();
    let snapshots = SnapshotPool::new(4, IN_FLIGHT).unwrap();
    let mut pool = GraphPool::new(graph_capacity(), IN_FLIGHT).unwrap();
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
    cpu_frames(&engine, &snapshots, &mut pool, &draws, WARMUP);
    let startup = Snapshot::now().since(start);
    assert!(
        startup.rust[renderer::measurement::RustEvent::Allocate as usize].calls != 0,
        "Rust global allocator hooks are absent or unexercised"
    );
    let report = cpu_frames(&engine, &snapshots, &mut pool, &draws, FRAMES);
    assert!(pool.is_idle());
    let before_shutdown = Snapshot::now();
    drop((pool, snapshots, engine));
    let shutdown = Snapshot::now().since(before_shutdown);
    assert!(
        shutdown.rust[renderer::measurement::RustEvent::Deallocate as usize].calls != 0,
        "Rust global deallocation hooks are absent or unexercised"
    );
    // Serialization, formatting, and all reporter-owned allocations are outside
    // both total and phase intervals. Prepared owner shutdown is separate.
    let json = serde_json::json!({
        "scenario": "cpu_snapshot_build_compile_return",
        "mode": if baseline { "baseline" } else { "require_zero" },
        "features": features(),
        "counter_semantics": semantics(),
        "warmup_frames": WARMUP,
        "measured_frames": FRAMES,
        "retained_frame_slots": IN_FLIGHT,
        "graph_capacity": capacity_report(),
        "draw_counts": [0, 1, 4, 2],
        "extra_compute_passes": [0, 1, 2],
        "boundary_cases": ["snapshot/graph busy", "draw/resource capacity", "unbuilt snapshot/uncompiled graph cancel"],
        "coverage": "Rust snapshot/build/compile and all normal-frame owners dropped; no native execution or presentation",
        "startup_positive_control": counts(startup),
        "shutdown_outside_normal_frames": counts(shutdown),
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
    use std::time::Duration;
    const GPU_WARMUP: usize = 24;
    const GPU_FRAMES: usize = 48;
    // Generic buffer provenance can retain old compiled input metadata after
    // GPU retirement, independently of the three pending/three CPU results.
    const GRAPH_SLOTS: usize = IN_FLIGHT * 4;

    // A measurement adapter only: both normal public APIs exercise the same
    // graph and ownership schedule. It exposes no renderer-internal operands.
    trait Backend {
        const RECORDING_CANCEL: bool;
        const DETACHED_REAP: bool;
        type Pending;
        type Completed;
        fn cancel_recording(&mut self, graph: &CompiledGraph);
        fn reject_capacity(&mut self, graph: &CompiledGraph);
        fn reap_detached(&mut self);
        fn collect_retired(&mut self);
        fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending;
        fn complete(pending: &mut Self::Pending) -> Self::Completed;
        fn consume(result: &mut Self::Completed, pass: PassHandle, expected: &[u8; 16]);
    }

    type Slot<T> = Option<(T, PassHandle, [u8; 16])>;

    fn graph(pool: &mut GraphPool, input: &[u8; 32], copies: usize) -> (Graph, PassHandle) {
        let mut graph = pool.begin().unwrap();
        let a = graph
            .buffer("memory source", BufferDesc { size: 32 })
            .unwrap();
        graph.upload_buffer("memory upload", a, 0, input).unwrap();
        let mut output = a;
        let mut read_offset = 0;
        if copies == 3 {
            graph
                .copy_buffer("memory self copy", a, 0, a, 16, 16)
                .unwrap();
            read_offset = 16;
        } else if copies != 0 {
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
            .readback_buffer("memory readback", output, read_offset, 16)
            .unwrap();
        (graph, read)
    }

    fn frames<B: Backend>(
        runtime: &mut B,
        pool: &mut GraphPool,
        inputs: &[[u8; 32]; 4],
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
            let version = (frame + frame / 4) % inputs.len();
            let copies = frame % 4;
            let (graph, pass) = phase(&mut phases[0], || graph(pool, &inputs[version], copies));
            let plan = phase(&mut phases[1], || graph.compile().unwrap());
            assert_eq!(plan.pass_count(), if copies == 3 { 3 } else { 2 + copies });
            let work = phase(&mut phases[2], || {
                runtime.cancel_recording(&plan);
                runtime.submit(&plan)
            });
            pending[slot] = Some((work, pass, expected[version]));
            if frame >= IN_FLIGHT - 1 {
                phase(&mut phases[2], || runtime.reject_capacity(&plan));
            }
            phase(&mut phases[4], || drop(plan));
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
        // An application may discard completed CPU output without reading it.
        // Returning that result must recycle its prepared range and metadata.
        let (discarded, _) = phase(&mut phases[0], || graph(pool, &inputs[0], 3));
        let discarded = phase(&mut phases[1], || discarded.compile().unwrap());
        let mut work = phase(&mut phases[2], || runtime.submit(&discarded));
        let result = phase(&mut phases[3], || B::complete(&mut work));
        phase(&mut phases[4], || drop((result, work, discarded)));
        let oversized = phase(&mut phases[0], || {
            let mut graph = pool.begin().unwrap();
            let buffer = graph
                .buffer("over capacity", BufferDesc { size: 64 })
                .unwrap();
            graph
                .upload_buffer("over capacity upload", buffer, 0, &inputs[0])
                .unwrap();
            graph
                .readback_buffer("over capacity readback", buffer, 0, 16)
                .unwrap();
            graph
        });
        let oversized = phase(&mut phases[1], || oversized.compile().unwrap());
        // The normal native working set reserves 32-byte and 16-byte buffers;
        // a new 64-byte class must fail without hidden growth or a GPU wait.
        phase(&mut phases[2], || runtime.reject_capacity(&oversized));
        phase(&mut phases[4], || drop(oversized));
        if B::DETACHED_REAP {
            let (graph, _) = phase(&mut phases[0], || graph(pool, &inputs[0], 3));
            let plan = phase(&mut phases[1], || graph.compile().unwrap());
            let detached = phase(&mut phases[2], || runtime.submit(&plan));
            phase(&mut phases[4], || drop(detached));
            phase(&mut phases[3], || runtime.reap_detached());
            phase(&mut phases[4], || drop(plan));
        }
        phase(&mut phases[4], || {
            runtime.collect_retired();
            recycle_graph_slots::<GRAPH_SLOTS>(pool);
        });
        Report {
            total: Snapshot::now().since(start),
            phases,
        }
    }

    fn inputs() -> ([[u8; 32]; 4], [[u8; 16]; 4]) {
        let data: [[u8; 32]; 4] = std::array::from_fn(|version| {
            std::array::from_fn(|byte| ((byte * 37 + version * 19) ^ (byte >> 1)) as u8)
        });
        (data, data.map(|bytes| bytes[..16].try_into().unwrap()))
    }

    fn positive_control(startup: Snapshot) {
        assert!(
            startup.rust[renderer::measurement::RustEvent::Allocate as usize].calls != 0,
            "Rust global allocator hooks are absent or unexercised"
        );
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
            intervals.iter().any(|interval| interval.rust
                [renderer::measurement::RustEvent::Deallocate as usize]
                .calls
                != 0),
            "Rust global deallocation hooks are absent or unexercised"
        );
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

    fn report<B: Backend>(
        name: &str,
        baseline: bool,
        startup: Snapshot,
        shutdown: Snapshot,
        result: Report<5>,
        retained_graph_slots: usize,
        native_capacity: serde_json::Value,
    ) {
        let json = serde_json::json!({
            "scenario": name,
            "mode": if baseline { "baseline" } else { "require_zero" },
            "features": features(),
            "counter_semantics": semantics(),
            "native_request": if cfg!(feature = "development-validation") { "Required" } else { "Disabled" },
            "warmup_frames": GPU_WARMUP,
            "measured_frames": GPU_FRAMES,
            "pending_capacity_exercised": IN_FLIGHT,
            "completed_readbacks_held": IN_FLIGHT,
            "graph_slots": GRAPH_SLOTS,
            "retained_provenance_graph_slots_after_window": retained_graph_slots,
            "graph_capacity": capacity_report(),
            "native_capacity": native_capacity,
            "input_versions": 4,
            "copy_passes": [0, 1, 2, 1],
            "last_case_is_disjoint_self_copy": true,
            "boundary_cases": {
                "native_busy_rejection": true,
                "unprepared_64byte_buffer_rejection": true,
                "readback_wrong_length_preserves_result": true,
                "unsubmitted_recording_cancel": B::RECORDING_CANCEL,
                "detached_submission_runtime_poll": B::DETACHED_REAP,
            },
            "detached_reap_submissions": usize::from(B::DETACHED_REAP),
            "completed_unconsumed_drop_submissions": 1,
            "frame_count_excludes_lifetime_boundary_submissions": true,
            "coverage": "public buffer Graph build/compile/submit/complete/readback/return; no raster/image/WSI",
            "caller": "fixed arrays and prepared input bytes; report outside measurement; explicit fence waits/polls",
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
        const RECORDING_CANCEL: bool = false;
        const DETACHED_REAP: bool = false;
        type Pending = renderer::Submission;
        type Completed = renderer::SubmissionResult;
        fn cancel_recording(&mut self, _: &CompiledGraph) {}
        fn reap_detached(&mut self) {}
        fn collect_retired(&mut self) {
            assert!(self.poll_frame().unwrap().is_none());
            assert_eq!(self.pending_frames(), 0);
        }
        fn reject_capacity(&mut self, graph: &CompiledGraph) {
            assert_eq!(
                self.submit(graph).err().unwrap().kind(),
                renderer::RuntimeErrorKind::CapacityExhausted
            );
        }
        fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending {
            self.submit(graph).unwrap()
        }
        fn complete(pending: &mut Self::Pending) -> Self::Completed {
            pending.wait(Duration::from_secs(30)).unwrap().unwrap()
        }
        fn consume(result: &mut Self::Completed, pass: PassHandle, expected: &[u8; 16]) {
            assert_eq!(result.readback_len(pass).unwrap(), 16);
            let mut wrong = [0xcc; 15];
            assert_eq!(
                result.readback_into(pass, &mut wrong).unwrap_err().kind(),
                renderer::RuntimeErrorKind::InvalidInput
            );
            assert_eq!(wrong, [0xcc; 15]);
            assert_eq!(result.readback_len(pass).unwrap(), 16);
            let mut bytes = [0; 16];
            result.readback_into(pass, &mut bytes).unwrap();
            assert_eq!(&bytes, expected);
        }
    }

    #[cfg(feature = "vulkan")]
    pub fn vulkan(baseline: bool) {
        if cfg!(debug_assertions) {
            panic!("memory acceptance requires --release");
        }
        let start = Snapshot::now();
        let config = renderer::RuntimeConfig {
            execution_slots: IN_FLIGHT,
            resource_capacity: 8,
            staging_bytes: 16 * 1024,
            staging_ranges: 32,
            max_shader_bindings: 8,
        };
        let mut runtime = renderer::VulkanRuntime::new(config).unwrap();
        assert_ne!(runtime.device_info().device_type, "CPU");
        let monitor = runtime.validation_monitor();
        let frame_capacity = renderer::FrameCapacity {
            graph: graph_capacity(),
            work_slots: IN_FLIGHT,
            result_slots: IN_FLIGHT * 2,
            metadata_slots: 16,
            range_snapshots: 32,
            initialized_ranges_per_resource: 8,
            provenance_segments_per_resource: 8,
        };
        runtime
            .prepare_working_set(
                frame_capacity,
                &[
                    renderer::ResourceClass::Buffer {
                        desc: BufferDesc { size: 32 },
                        count: IN_FLIGHT,
                    },
                    renderer::ResourceClass::Buffer {
                        desc: BufferDesc { size: 16 },
                        count: IN_FLIGHT,
                    },
                ],
            )
            .unwrap();
        let (inputs, expected) = inputs();
        let mut pool = GraphPool::new(graph_capacity(), GRAPH_SLOTS).unwrap();
        frames(&mut runtime, &mut pool, &inputs, &expected, GPU_WARMUP);
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
        let measured = frames(&mut runtime, &mut pool, &inputs, &expected, GPU_FRAMES);
        let retained_graph_slots = pool.leased_slots();
        assert_eq!(
            retained_graph_slots, 0,
            "unregistered inputs returned every Graph pin"
        );
        let before_shutdown = Snapshot::now();
        drop((runtime, pool));
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
        report::<renderer::VulkanRuntime>(
            "vulkan_buffer_full_lifecycle",
            baseline,
            startup,
            shutdown,
            measured,
            retained_graph_slots,
            serde_json::json!({
                "execution_slots": config.execution_slots,
                "resource_capacity": config.resource_capacity,
                "staging_bytes": config.staging_bytes,
                "staging_ranges": config.staging_ranges,
                "max_shader_bindings": config.max_shader_bindings,
                "work_slots": frame_capacity.work_slots,
                "result_slots": frame_capacity.result_slots,
                "metadata_slots": frame_capacity.metadata_slots,
                "range_snapshots": frame_capacity.range_snapshots,
                "initialized_ranges_per_resource": frame_capacity.initialized_ranges_per_resource,
                "provenance_segments_per_resource": frame_capacity.provenance_segments_per_resource,
                "additional_buffer_classes": [{ "bytes": 32, "instances": IN_FLIGHT }, { "bytes": 16, "instances": IN_FLIGHT }],
            }),
        );
    }

    #[cfg(all(feature = "d3d12", windows))]
    impl Backend for renderer::D3D12Runtime {
        const RECORDING_CANCEL: bool = true;
        const DETACHED_REAP: bool = true;
        type Pending = renderer::D3D12Submission;
        type Completed = renderer::D3D12SubmissionResult;
        fn cancel_recording(&mut self, graph: &CompiledGraph) {
            drop(self.prepare(graph).unwrap());
        }
        fn reject_capacity(&mut self, graph: &CompiledGraph) {
            assert_eq!(
                self.prepare(graph).err().unwrap().kind(),
                renderer::D3D12RuntimeErrorKind::CapacityExhausted
            );
        }
        fn reap_detached(&mut self) {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                if self.poll().unwrap() == 1 {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "detached frame timed out"
                );
                std::thread::yield_now();
            }
        }
        fn collect_retired(&mut self) {
            assert_eq!(self.poll().unwrap(), 0);
        }
        fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending {
            self.submit(graph).unwrap()
        }
        fn complete(pending: &mut Self::Pending) -> Self::Completed {
            pending.wait(Duration::from_secs(30)).unwrap().unwrap()
        }
        fn consume(result: &mut Self::Completed, pass: PassHandle, expected: &[u8; 16]) {
            assert_eq!(result.readback_len(pass).unwrap(), 16);
            let mut wrong = [0xcc; 15];
            assert_eq!(
                result.readback_into(pass, &mut wrong).unwrap_err().kind(),
                renderer::D3D12RuntimeErrorKind::InvalidInput
            );
            assert_eq!(wrong, [0xcc; 15]);
            assert_eq!(result.readback_len(pass).unwrap(), 16);
            let mut bytes = [0; 16];
            result.readback_into(pass, &mut bytes).unwrap();
            assert_eq!(&bytes, expected);
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
        config.capacity.resources = 2;
        config.capacity.operations = 4;
        config.capacity.buffers = 2;
        config.capacity.buffer_bytes = 32;
        config.capacity.upload_bytes = 32;
        config.capacity.readback_bytes = 16;
        config.capacity.scratch_bytes = 16;
        let mut runtime = renderer::D3D12Runtime::new(config).unwrap();
        let monitor = runtime.validation_monitor();
        let (inputs, expected) = inputs();
        let mut pool = GraphPool::new(graph_capacity(), GRAPH_SLOTS).unwrap();
        frames(&mut runtime, &mut pool, &inputs, &expected, GPU_WARMUP);
        let startup = Snapshot::now().since(start);
        positive_control(startup);
        let measured = frames(&mut runtime, &mut pool, &inputs, &expected, GPU_FRAMES);
        let retained_graph_slots = pool.leased_slots();
        let before_shutdown = Snapshot::now();
        runtime.shutdown().unwrap();
        drop(pool);
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
        report::<renderer::D3D12Runtime>(
            "d3d12_buffer_full_lifecycle",
            baseline,
            startup,
            shutdown,
            measured,
            retained_graph_slots,
            serde_json::json!({
                "executions": config.capacity.executions,
                "results": config.capacity.results,
                "resources": config.capacity.resources,
                "operations": config.capacity.operations,
                "buffers": config.capacity.buffers,
                "buffer_bytes": config.capacity.buffer_bytes,
                "upload_bytes": config.capacity.upload_bytes,
                "readback_bytes": config.capacity.readback_bytes,
                "scratch_bytes": config.capacity.scratch_bytes,
            }),
        );
    }
}

#[cfg(all(feature = "d3d12", windows))]
pub use buffer::d3d12 as d3d12_buffer_lifecycle;
#[cfg(feature = "vulkan")]
pub use buffer::vulkan as vulkan_buffer_lifecycle;

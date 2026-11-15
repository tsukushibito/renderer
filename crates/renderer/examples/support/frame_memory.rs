//! Fixed-storage measurement driver. Report construction is outside the window.

use renderer::engine::{
    Draw, Engine, FrameGraph, FrameSnapshot, IDENTITY, Material, MeshData, Vertex, View,
};
use renderer::graph::{CompiledGraph, ComputeTransform, ImageDesc};
use renderer::measurement::Snapshot;

const IN_FLIGHT: usize = 3;
const WARMUP: usize = 12;
const FRAMES: usize = 24;

struct Report {
    total: Snapshot,
    phases: [Snapshot; 4],
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

fn cpu_frames(engine: &Engine, draws: &[Draw; 4], count: usize) -> Report {
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

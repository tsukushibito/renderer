//! Whole MeshRegistry image frames through the ordinary public, prepared API.
//! Resident uploads and capacity preparation are setup; all frame returns are
//! measured. This headless probe does not exercise presentation.

use super::frame_memory::{
    Report, capacity_report, counts, features, graph_capacity as base_graph_capacity, phase,
    semantics,
};
use super::image_transform::{self, Transform};
use renderer::geometry::{MeshCapacity, MeshRegistry, SnapshotPool};
use renderer::graph::{Attachment, ImageDesc, LoadOp, PassHandle, StoreOp};
use renderer::measurement::{HostEvent, NativeEvent, RustEvent, Snapshot};
use renderer::program::ManagedRasterProgram;
use renderer::vulkan::{
    EngineFrameCapacity, EngineRenderError, FrameCapacity, FrameCompletion, MeshCacheConfig,
    MeshPreparationError, MeshUploadState, PreparedEngineFrame, RenderedFrame, ResourceClass,
    RuntimeConfig, ValidationMode, VulkanEngine, VulkanRuntime,
};
use renderer_pipeline::unlit_forward::{Draw, FrameSnapshot, IDENTITY, Vertex, View, mesh_data};
use renderer_pipeline::{passes, unlit_forward};
use std::time::{Duration, Instant};

const IN_FLIGHT: usize = 3;
const GRAPH_SLOTS: usize = 12;
const WARMUP: usize = 24;
const FRAMES: usize = 48;
const WIDTH: u32 = 16;
const HEIGHT: u32 = 16;
const PIXEL_BYTES: usize = WIDTH as usize * HEIGHT as usize * 4;
const WAIT: Duration = Duration::from_secs(30);

fn graph_capacity() -> renderer::graph::GraphCapacity {
    // Four objects need eight captured draws when depth and color are separate.
    renderer::graph::GraphCapacity {
        draws: 8,
        ..base_graph_capacity()
    }
}

#[derive(Clone, Copy)]
struct Expected {
    left: [u8; 4],
    right: [u8; 4],
    background: [u8; 4],
}

struct Pending {
    frame: RenderedFrame,
    expected: Expected,
}

struct Held {
    completion: FrameCompletion,
    pass: PassHandle,
    expected: Expected,
}

fn build(
    renderer: &mut VulkanEngine,
    snapshot: &FrameSnapshot,
    invert_passes: usize,
    readback: bool,
) -> PreparedEngineFrame {
    let mut prepared = renderer.begin_graph().unwrap();
    let inputs = renderer.connect_snapshot(&mut prepared, snapshot).unwrap();
    let graph = &mut prepared.graph;
    let mut color = if invert_passes == 1 {
        // Exercise direct public composition as well as the optional recipe,
        // including equal-depth shading and zero-draw clear/load frames.
        let color = graph
            .image("memory color", ImageDesc::rgba8(WIDTH, HEIGHT))
            .unwrap();
        let depth = graph
            .image("memory depth", ImageDesc::depth32(WIDTH, HEIGHT))
            .unwrap();
        let depth_view = graph.image_view(depth, 0, 0).unwrap();
        passes::depth_prepass(
            graph,
            "memory prepass",
            inputs.draws(),
            Attachment {
                view: depth_view,
                load: LoadOp::Clear(1.0),
                store: StoreOp::Store,
            },
        )
        .unwrap();
        passes::opaque(
            graph,
            "memory opaque",
            inputs.draws(),
            Attachment {
                view: graph.image_view(color, 0, 0).unwrap(),
                load: LoadOp::Clear(snapshot.view().clear_color),
                store: StoreOp::Store,
            },
            Some(Attachment {
                view: depth_view,
                load: LoadOp::Load,
                store: StoreOp::Store,
            }),
            passes::OpaqueDepth::Prepassed,
        )
        .unwrap();
        color
    } else {
        unlit_forward::forward(
            graph,
            "memory forward",
            inputs.draws(),
            unlit_forward::ForwardConfig {
                color: ImageDesc::rgba8(WIDTH, HEIGHT),
                clear_color: snapshot.view().clear_color,
                depth: true,
            },
        )
        .unwrap()
        .color
    };
    for _ in 0..invert_passes {
        let output = graph
            .image("memory image transform", ImageDesc::rgba8(WIDTH, HEIGHT))
            .unwrap();
        image_transform::dispatch(
            graph,
            "memory image transform",
            color,
            output,
            Transform::Invert,
        )
        .unwrap();
        color = output;
    }
    drop(inputs);
    let output = graph
        .image("memory standard output", ImageDesc::rgba8(WIDTH, HEIGHT))
        .unwrap();
    graph
        .copy_image("memory standard output", color, output)
        .unwrap();
    prepared.select_output(output, readback).unwrap()
}

fn cancel_and_exhaust(renderer: &mut VulkanEngine, snapshot: &FrameSnapshot) {
    let prepared: [PreparedEngineFrame; IN_FLIGHT] =
        std::array::from_fn(|_| build(renderer, snapshot, 2, false));
    let [first, second, third] = prepared;
    let compiled = first.compile().unwrap();
    // Both compiled and mutable frames retain the same prepared-frame pins.
    // Exhaustion must reject the next checkout without growing or waiting.
    assert!(matches!(
        renderer.begin_graph(),
        Err(EngineRenderError::Preparation(
            MeshPreparationError::FrameCapacityExhausted
        ))
    ));
    drop((compiled, second, third));
}

fn complete_one(
    renderer: &mut VulkanEngine,
    pending: &mut [Option<Pending>; IN_FLIGHT],
    held: &mut [Option<Held>; IN_FLIGHT],
) {
    let completion = renderer.runtime_mut().wait_frame(WAIT).unwrap().unwrap();
    let index = pending
        .iter()
        .position(|entry| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.frame.submission.id() == completion.id())
        })
        .expect("completion belongs to one of the fixed pending frames");
    let Pending {
        mut frame,
        expected,
    } = pending[index].take().unwrap();
    let pass = frame.readback.expect("image acceptance requests readback");
    renderer.release_output(&mut frame).unwrap();
    drop(frame);
    let target = held.iter_mut().find(|entry| entry.is_none()).unwrap();
    *target = Some(Held {
        completion,
        pass,
        expected,
    });
}

fn consume(mut held: Held) {
    assert_eq!(
        held.completion.result.readback_len(held.pass).unwrap(),
        PIXEL_BYTES
    );
    let mut pixels = [0; PIXEL_BYTES];
    held.completion
        .result
        .readback_into(held.pass, &mut pixels)
        .unwrap();
    // At y=8, these pixels lie inside only the corresponding translated
    // triangle. Check the transform's effect, not just changing its input bytes.
    for (x, expected) in [(5, held.expected.left), (10, held.expected.right)] {
        let offset = ((HEIGHT as usize / 2) * WIDTH as usize + x) * 4;
        assert_eq!(&pixels[offset..offset + 4], &expected);
    }
    // Every corner lies outside the translated triangle. Invert preserves A.
    for corner in [
        0,
        (WIDTH as usize - 1) * 4,
        PIXEL_BYTES - WIDTH as usize * 4,
        PIXEL_BYTES - 4,
    ] {
        assert_eq!(&pixels[corner..corner + 4], &held.expected.background);
    }
    drop(held);
}

fn frames(
    engine: &mut MeshRegistry,
    snapshots: &SnapshotPool,
    renderer: &mut VulkanEngine,
    draws: &mut [Draw; 4],
    count: usize,
) -> Report<6> {
    let mut pending: [Option<Pending>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut held: [Option<Held>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut phases = [Snapshot::default(); 6];
    let start = Snapshot::now();
    let mut returned = 0;
    for frame in 0..count {
        if pending.iter().all(Option::is_some) {
            phase(&mut phases[5], || {
                if let Some(result) = held[returned % IN_FLIGHT].take() {
                    consume(result);
                }
                returned += 1;
            });
            phase(&mut phases[4], || {
                complete_one(renderer, &mut pending, &mut held)
            });
        }
        let draw_count = [0, 1, 4, 2][frame % 4];
        let invert_passes = frame % 3;
        let green = (frame / 4) % 2 != 0;
        let shifted_left = (frame / 2) % 2 == 0;
        let tint = if green {
            [0.0, 1.0, 0.0, 1.0]
        } else {
            [1.0, 0.0, 0.0, 1.0]
        };
        let tint_bytes = if green {
            [0, 255, 0, 255]
        } else {
            [255, 0, 0, 255]
        };
        let background = [0, 0, if frame % 2 == 0 { 0 } else { 255 }, 255];
        let mut expected = Expected {
            left: if draw_count != 0 && shifted_left {
                tint_bytes
            } else {
                background
            },
            right: if draw_count != 0 && !shifted_left {
                tint_bytes
            } else {
                background
            },
            background,
        };
        if invert_passes % 2 != 0 {
            for color in [
                &mut expected.left,
                &mut expected.right,
                &mut expected.background,
            ] {
                for channel in &mut color[..3] {
                    *channel = 255 - *channel;
                }
            }
        }
        let snapshot = phase(&mut phases[0], || {
            for draw in draws.iter_mut() {
                draw.tint = tint;
                draw.world_from_model = IDENTITY;
                draw.world_from_model[3][0] = if shifted_left { -0.25 } else { 0.25 };
            }
            FrameSnapshot::capture(
                engine,
                snapshots,
                &View {
                    width: WIDTH,
                    height: HEIGHT,
                    clip_from_world: IDENTITY,
                    clear_color: [0.0, 0.0, (frame % 2) as f32, 1.0],
                },
                &draws[..draw_count],
            )
            .unwrap()
        });
        phase(&mut phases[1], || cancel_and_exhaust(renderer, &snapshot));
        let prepared = phase(&mut phases[1], || {
            build(renderer, &snapshot, invert_passes, true)
        });
        let compiled = phase(&mut phases[2], || prepared.compile().unwrap());
        assert_eq!(
            compiled.graph.pass_count(),
            3 + invert_passes + usize::from(invert_passes == 1)
        );
        let rendered = phase(&mut phases[3], || renderer.submit_frame(compiled).unwrap());
        let target = pending.iter_mut().find(|entry| entry.is_none()).unwrap();
        *target = Some(Pending {
            frame: rendered,
            expected,
        });
        if frame >= IN_FLIGHT * 2 - 1 {
            assert!(pending.iter().all(Option::is_some));
            assert!(held.iter().all(Option::is_some));
        }
        phase(&mut phases[5], || drop(snapshot));
    }
    phase(&mut phases[5], || {
        for entry in &mut held {
            if let Some(result) = entry.take() {
                consume(result);
            }
        }
    });
    phase(&mut phases[4], || {
        while pending.iter().any(Option::is_some) {
            complete_one(renderer, &mut pending, &mut held);
        }
    });
    phase(&mut phases[5], || {
        for result in held.into_iter().flatten() {
            consume(result);
        }
        drop(pending);
        // Also collect physical leases returned by the last CPU result. Native
        // working-set owners stay prepared; no frame result escapes the window.
        assert!(renderer.runtime_mut().poll_frame().unwrap().is_none());
        assert_eq!(renderer.runtime().pending_frames(), 0);
    });
    Report {
        total: Snapshot::now().since(start),
        phases,
    }
}

fn controls(startup: Snapshot, measured: Snapshot, shutdown: Snapshot) {
    assert!(
        startup.rust[..RustEvent::Deallocate as usize]
            .iter()
            .any(|event| event.calls != 0),
        "the selected Rust allocator must observe setup allocation"
    );
    assert_ne!(
        shutdown.rust[RustEvent::Deallocate as usize].calls,
        0,
        "the selected Rust allocator must observe shutdown deallocation"
    );
    for event in [
        NativeEvent::ObjectCreate,
        NativeEvent::ResourceCreate,
        NativeEvent::MemoryAllocate,
    ] {
        assert_ne!(
            startup.native[event as usize], 0,
            "startup native hook must be exercised"
        );
    }
    for event in [
        NativeEvent::ObjectDestroy,
        NativeEvent::ResourceDestroy,
        NativeEvent::MemoryFree,
    ] {
        assert!(
            [startup, measured, shutdown]
                .iter()
                .any(|interval| interval.native[event as usize] != 0),
            "native destruction hook must be exercised"
        );
    }
    assert_ne!(startup.vma_host[HostEvent::Allocate as usize].calls, 0);
    assert_ne!(shutdown.vma_host[HostEvent::Deallocate as usize].calls, 0);
}

pub fn lifecycle(baseline: bool) {
    if cfg!(debug_assertions) {
        panic!("memory acceptance requires --release");
    }
    let start = Snapshot::now();
    let mut engine = MeshRegistry::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })
    .unwrap();
    let snapshots = SnapshotPool::new(4, IN_FLIGHT).unwrap();
    let mesh = engine
        .create_mesh(
            mesh_data(
                &[[-0.4, -0.4, 0.25], [0.4, -0.4, 0.25], [0.0, 0.4, 0.25]].map(|position| Vertex {
                    position,
                    color: [1.0; 4],
                }),
                &[0, 1, 2],
            )
            .unwrap(),
        )
        .unwrap();
    let tint = [1.0; 4];
    let mut draws = [Draw {
        mesh,
        tint,
        world_from_model: IDENTITY,
    }; 4];
    let runtime = VulkanRuntime::new(RuntimeConfig {
        execution_slots: IN_FLIGHT,
        resource_capacity: 32,
        staging_bytes: 64 * 1024,
        staging_ranges: 64,
        max_shader_bindings: 8,
    })
    .unwrap();
    assert_ne!(runtime.device_info().device_type, "CPU");
    let monitor = runtime.validation_monitor();
    let mut renderer = VulkanEngine::new(
        runtime,
        MeshCacheConfig {
            max_meshes: 1,
            max_bytes: 1024,
            max_upload_submissions: 1,
            max_retired_sources: 1,
        },
        EngineFrameCapacity {
            prepared_frames: IN_FLIGHT,
            graph_slots: GRAPH_SLOTS,
            graph: graph_capacity(),
        },
    )
    .unwrap();
    // Mesh preparation submits ordinary upload Graphs too. Reserve runtime
    // work/result backing before those setup submissions; no lazy fallback is
    // available on the ordinary submit path.
    renderer
        .runtime_mut()
        .prepare_working_set(
            FrameCapacity {
                graph: graph_capacity(),
                work_slots: IN_FLIGHT,
                result_slots: IN_FLIGHT * 2,
                metadata_slots: 16,
                range_snapshots: 1024,
                initialized_ranges_per_resource: 32,
                provenance_segments_per_resource: 64,
                index_prefixes_per_resource: 64,
            },
            &[
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(WIDTH, HEIGHT),
                    count: 12,
                },
                ResourceClass::Image {
                    desc: ImageDesc::depth32(WIDTH, HEIGHT),
                    count: IN_FLIGHT,
                },
            ],
        )
        .unwrap();
    renderer
        .runtime_mut()
        .prepare_compute_programs(&[image_transform::program(Transform::Invert)])
        .unwrap();
    renderer
        .runtime_mut()
        .prepare_raster_programs(&[
            ManagedRasterProgram::mesh(),
            ManagedRasterProgram::depth_only(),
            ManagedRasterProgram::mesh_prepassed(),
        ])
        .unwrap();
    let resident = FrameSnapshot::capture(
        &engine,
        &snapshots,
        &View {
            width: WIDTH,
            height: HEIGHT,
            clip_from_world: IDENTITY,
            clear_color: [0.0, 0.0, 0.0, 1.0],
        },
        &draws,
    )
    .unwrap();
    let mut upload = renderer.prepare_snapshot(&resident, 2).unwrap();
    let upload_start = Instant::now();
    while upload.state != MeshUploadState::Ready || upload.pending_submissions != 0 {
        assert!(
            upload_start.elapsed() < WAIT,
            "resident mesh preparation timed out"
        );
        upload = renderer.wait_uploads(WAIT).unwrap();
    }
    drop(resident);
    frames(&mut engine, &snapshots, &mut renderer, &mut draws, WARMUP);
    let startup = Snapshot::now().since(start);
    let measured = frames(&mut engine, &snapshots, &mut renderer, &mut draws, FRAMES);
    let before_shutdown = Snapshot::now();
    drop((renderer, snapshots, engine));
    let shutdown = Snapshot::now().since(before_shutdown);
    controls(startup, measured.total, shutdown);
    let validation = monitor.report();
    assert!(validation.is_clean(), "{validation:#?}");
    if cfg!(feature = "development-validation") {
        assert_eq!(validation.core_validation, Some(true));
        assert_eq!(validation.synchronization_validation, Some(true));
    } else {
        assert_eq!(validation.requested, ValidationMode::Disabled);
    }
    let mut capacity = capacity_report();
    capacity["draws"] = graph_capacity().draws.into();
    let json = serde_json::json!({
        "scenario": "vulkan_engine_image_full_lifecycle",
        "mode": if baseline { "baseline" } else { "require_zero" },
        "features": features(),
        "counter_semantics": semantics(),
        "warmup_frames": WARMUP,
        "measured_frames": FRAMES,
        "extent": [WIDTH, HEIGHT],
        "pending_frames": IN_FLIGHT,
        "completed_readbacks_held": IN_FLIGHT,
        "engine_prepared_frames": IN_FLIGHT,
        "engine_graph_slots": GRAPH_SLOTS,
        "graph_capacity": capacity,
        "additional_working_set_images": { "rgba8": 12, "depth32": IN_FLIGHT },
        "draw_counts": [0, 1, 4, 2],
        "invert_passes": [0, 1, 2],
        "composition_routes": ["forward then custom Compute", "depth_prepass then opaque Prepassed", "explicit output copy"],
        "prepass_when_invert_passes": 1,
        "x_translations": [-0.25, 0.25],
        "verified_pixels": { "left": [5, 8], "right": [10, 8], "background": "four corners" },
        "boundary_cases": ["compiled/uncompiled prepared cancel", "prepared frame capacity/busy rejection"],
        "coverage": "ordinary public MeshRegistry snapshot/connection/build/compile, public standard forward/post and direct prepass/opaque, image Compute/output copy, runtime frame completion, caller readback_into and full result/output return; no WSI or asset churn",
        "caller": "fixed pending/result arrays and 1024-byte output; explicit per-frame fence waits only when pending array fills; report outside measurement",
        "unobserved": "driver/OS/native allocations outside the configured callbacks",
        "startup_positive_control": counts(startup),
        "shutdown_outside_normal_frames": counts(shutdown),
        "total": counts(measured.total),
        "phases_snapshot_build_compile_submit_complete_return": measured.phases.map(counts),
    });
    println!("FRAME_MEMORY_REPORT={json}");
    assert!(!measured.total.overflowed);
    if !baseline {
        assert!(
            measured.total.is_allocation_free(),
            "MeshRegistry image lifecycle still allocates or frees: {:?}",
            measured.total
        );
    }
}

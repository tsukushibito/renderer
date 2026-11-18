//! Caller-composed Graphs keep resident inputs and output authority through
//! cancellation, submission and native completion. Missing GPU prerequisites
//! fail these explicit acceptance tests rather than silently skipping them.

#![cfg(all(
    feature = "vulkan",
    any(
        feature = "development-validation",
        not(any(feature = "vulkan-diagnostics", feature = "graph-diagnostics"))
    )
))]

use renderer::ImageFormat;
use std::time::{Duration, Instant};

use renderer::engine::{
    Draw, Engine, EngineCapacity, FrameOutputOwnership, FrameSnapshot, IDENTITY, Material,
    MeshData, SnapshotPool, Vertex, View,
};
use renderer::graph::{
    Attachment, Binding, BindingResource, BufferDesc, Graph, GraphCapacity, GraphErrorKind,
    ImageDesc, ImageHandle, LoadOp, PassHandle, RasterAttachments, StoreOp,
};
use renderer::program::{ManagedProgram, ManagedRasterProgram};
use renderer::{
    EngineFrameCapacity, EngineRenderError, FrameCapacity, FrameId, MeshCacheConfig,
    MeshPreparationError, MeshUploadState, ResourceClass, RuntimeConfig, ValidationMode,
    VulkanEngine, VulkanRuntime,
};

const WAIT: Duration = Duration::from_secs(30);
const PREPARED_FRAMES: usize = 4;

fn capacity() -> GraphCapacity {
    GraphCapacity {
        resources: 24,
        passes: 24,
        accesses: 96,
        draws: 8,
        bindings: 16,
        attachments: 16,
        dependencies: 16,
        initial_ranges: 64,
        name_bytes: 2048,
        upload_bytes: 512,
        edges: 256,
        segments: 256,
        readers: 256,
    }
}

fn hdr(width: u32, height: u32) -> ImageDesc {
    ImageDesc {
        format: ImageFormat::Rgba16Float,
        ..ImageDesc::rgba8(width, height)
    }
}

fn quad(color: [f32; 4]) -> MeshData {
    MeshData::new(
        &[
            [-1.0, -1.0, 0.5],
            [1.0, -1.0, 0.5],
            [-1.0, 1.0, 0.5],
            [1.0, 1.0, 0.5],
        ]
        .map(|position| Vertex { position, color }),
        &[0, 1, 2, 2, 1, 3],
    )
    .unwrap()
}

fn prepare(engine: &mut VulkanEngine, snapshot: &FrameSnapshot) {
    let deadline = Instant::now() + WAIT;
    let mut upload = engine.prepare_snapshot(snapshot, 0).unwrap();
    while upload.state != MeshUploadState::Ready || upload.pending_submissions != 0 {
        upload = engine
            .wait_uploads(
                deadline
                    .checked_duration_since(Instant::now())
                    .expect("resident input preparation timed out"),
            )
            .unwrap();
    }
}

fn check_readbacks(engine: &mut VulkanEngine, id: FrameId, expected: &[(PassHandle, Vec<u8>)]) {
    let mut completion = engine
        .runtime_mut()
        .wait_frame(WAIT)
        .unwrap()
        .expect("caller-composed Graph timed out");
    assert_eq!(completion.id(), id);
    for (pass, expected) in expected {
        assert_eq!(
            completion.result.readback_len(*pass).unwrap(),
            expected.len()
        );
        let mut actual = vec![0; expected.len()];
        completion.result.readback_into(*pass, &mut actual).unwrap();
        assert_eq!(&actual, expected);
    }
}

fn attachment(graph: &Graph, target: ImageHandle) -> Attachment<[f32; 4]> {
    Attachment {
        view: graph.image_view(target, 0, 0).unwrap(),
        load: LoadOp::Clear([0.0; 4]),
        store: StoreOp::Store,
    }
}

fn half_pixels(pixel: [u16; 4], count: usize) -> Vec<u8> {
    let bytes: Vec<_> = pixel.into_iter().flat_map(u16::to_le_bytes).collect();
    bytes.repeat(count)
}

fn without_snapshot(engine: &mut VulkanEngine) {
    let mut frame = engine.begin_graph().unwrap();
    let graph = &mut frame.graph;
    let source = graph
        .buffer("caller storage input", BufferDesc { size: 16 })
        .unwrap();
    let computed = graph
        .buffer("caller compute output", BufferDesc { size: 16 })
        .unwrap();
    let copied = graph
        .buffer("caller copied output", BufferDesc { size: 16 })
        .unwrap();
    let bytes: Vec<_> = [2.0_f32, 4.0, 8.0, 1.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    graph
        .upload_buffer("caller input bytes", source, 0, &bytes)
        .unwrap();
    graph
        .dispatch(
            "caller Compute without snapshot",
            &ManagedProgram::storage_readback(),
            &[
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: source,
                        offset: 0,
                        size: 16,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: computed,
                        offset: 0,
                        size: 16,
                    },
                },
            ],
            [1, 1, 1],
        )
        .unwrap();
    graph
        .copy_buffer("caller copy", computed, 0, copied, 0, 16)
        .unwrap();
    graph.export_buffer(computed).unwrap();
    graph.export_buffer(copied).unwrap();
    let pass = graph
        .readback_buffer("caller copied bytes", copied, 0, 16)
        .unwrap();
    let plan = frame.compile().unwrap();
    assert_eq!(plan.pass_count(), 4, "no implicit image, draw or readback");
    let submitted = engine.submit_graph(plan).unwrap();
    for handle in [computed, copied] {
        let exported = submitted.export_buffer(handle).unwrap();
        engine.runtime_mut().delete(exported.key()).unwrap();
    }
    check_readbacks(engine, submitted.id(), &[(pass, bytes)]);
    assert_eq!(engine.runtime().stats().registered_resources, 0);
}

fn resident_versions(engine: &mut VulkanEngine) {
    let mut assets = Engine::new(EngineCapacity {
        meshes: 1,
        materials: 1,
        retired_mesh_versions: 3,
    })
    .unwrap();
    let snapshots = SnapshotPool::new(1, 2).unwrap();
    let mesh = assets.create_mesh(quad([2.0, 0.0, 0.0, 1.0])).unwrap();
    let material = assets.create_material(Material::default()).unwrap();
    let draw = Draw {
        mesh,
        material,
        world_from_model: IDENTITY,
    };
    let view = View {
        width: 11,
        height: 9,
        clip_from_world: IDENTITY,
        clear_color: [0.0; 4],
    };
    let old = assets.snapshot(&snapshots, &view, &[draw]).unwrap();
    let mut not_ready = engine.begin_graph().unwrap();
    assert!(matches!(
        engine.connect_snapshot(&mut not_ready, &old),
        Err(EngineRenderError::Preparation(
            MeshPreparationError::NotResident
        ))
    ));
    drop(not_ready);
    prepare(engine, &old);
    assets
        .update_mesh(mesh, quad([0.0, 4.0, 0.0, 1.0]))
        .unwrap();
    let other_view = View {
        width: 5,
        height: 13,
        ..view
    };
    let new = assets.snapshot(&snapshots, &other_view, &[draw]).unwrap();
    prepare(engine, &new);

    let mut cancelled = engine.begin_graph().unwrap();
    let inputs = engine.connect_snapshot(&mut cancelled, &old).unwrap();
    assert!(
        matches!(engine.connect_snapshot(&mut cancelled, &old), Err(EngineRenderError::Graph(error)) if error.kind() == GraphErrorKind::CapacityExhausted)
    );
    drop(inputs);
    let cancelled = cancelled.compile().unwrap();
    assert_eq!(
        cancelled.pass_count(),
        0,
        "connection adds no drawing or output"
    );
    drop(cancelled);
    assert_eq!(engine.mesh_cache_stats().prepared_retained_meshes, 0);

    let mut frame = engine.begin_graph().unwrap();
    let old_inputs = engine.connect_snapshot(&mut frame, &old).unwrap();
    let new_inputs = engine.connect_snapshot(&mut frame, &new).unwrap();
    assert_eq!(*old_inputs.view(), view);
    assert_eq!(*new_inputs.view(), other_view);
    let (vertices, indices) = {
        let draw = old_inputs.draws().next().unwrap();
        (draw.vertices, draw.indices)
    };
    let graph = &mut frame.graph;
    assert_eq!(
        graph
            .upload_buffer("reject resident write", vertices, 0, &[0; 4])
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    assert_eq!(
        graph.export_buffer(indices).unwrap_err().kind(),
        GraphErrorKind::InvalidInput
    );
    let copied_indices = graph
        .buffer("independent index copy", BufferDesc { size: 24 })
        .unwrap();
    graph
        .copy_buffer("copy readonly indices", indices, 0, copied_indices, 0, 24)
        .unwrap();
    graph.export_buffer(copied_indices).unwrap();
    let index_readback = graph
        .readback_buffer("copied index bytes", copied_indices, 0, 24)
        .unwrap();
    let first = graph.image("old View HDR", hdr(6, 4)).unwrap();
    let second = graph.image("new View HDR", hdr(3, 2)).unwrap();
    graph
        .raster(
            "old mesh version",
            RasterAttachments {
                color_resolves: &[],
                colors: &[attachment(graph, first)],
                depth: None,
            },
            |raster| raster.draw(&ManagedRasterProgram::mesh_hdr(), old_inputs.draws()),
        )
        .unwrap();
    graph
        .raster(
            "new mesh version",
            RasterAttachments {
                color_resolves: &[],
                colors: &[attachment(graph, second)],
                depth: None,
            },
            |raster| raster.draw(&ManagedRasterProgram::mesh_hdr(), new_inputs.draws()),
        )
        .unwrap();
    graph.export_image(first).unwrap();
    graph.export_image(second).unwrap();
    let first_pass = graph.readback_image("old View pixels", first).unwrap();
    let second_pass = graph.readback_image("new View pixels", second).unwrap();
    drop((old_inputs, new_inputs));
    drop((old, new));
    assets.delete_mesh(mesh).unwrap();
    assets.delete_material(material).unwrap();
    let plan = frame.compile().unwrap();
    engine.unload_mesh(mesh);
    engine.collect_retired_meshes(Some(&assets)).unwrap();
    assert_eq!(engine.mesh_cache_stats().prepared_retained_meshes, 2);
    let submitted = engine.submit_graph(plan).unwrap();
    assert_eq!(engine.mesh_cache_stats().prepared_retained_meshes, 0);
    for handle in [first, second] {
        engine
            .runtime_mut()
            .delete(submitted.export_image(handle).unwrap().key())
            .unwrap();
    }
    engine
        .runtime_mut()
        .delete(submitted.export_buffer(copied_indices).unwrap().key())
        .unwrap();
    check_readbacks(
        engine,
        submitted.id(),
        &[
            (first_pass, half_pixels([0x4000, 0, 0, 0x3c00], 24)),
            (second_pass, half_pixels([0, 0x4400, 0, 0x3c00], 6)),
            (
                index_readback,
                [0_u32, 1, 2, 2, 1, 3]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect(),
            ),
        ],
    );
    engine.collect_retired_meshes(Some(&assets)).unwrap();
    engine.release_idle_inputs();
    assets.collect_retired_meshes();
    assert_eq!(engine.mesh_cache_stats().resident_meshes, 0);
}

fn output_authority(engine: &mut VulkanEngine) {
    let mut frame = engine.begin_graph().unwrap();
    let caller_handle = frame
        .graph
        .image("caller output", ImageDesc::rgba8(5, 3))
        .unwrap();
    let caller_bytes = [17, 34, 51, 255].repeat(15);
    frame
        .graph
        .upload_image("caller output contents", caller_handle, &caller_bytes)
        .unwrap();
    frame.graph.export_image(caller_handle).unwrap();
    let submitted = engine.submit_graph(frame.compile().unwrap()).unwrap();
    let caller = submitted.export_image(caller_handle).unwrap();
    check_readbacks(engine, submitted.id(), &[]);

    let mut frame = engine.begin_graph().unwrap();
    let imported = frame
        .graph
        .import_image("caller selected output", &caller)
        .unwrap();
    let selected = frame.select_output(imported, true).unwrap();
    assert_eq!(selected.output_ownership(), FrameOutputOwnership::Caller);
    let mut rendered = engine.submit_frame(selected.compile().unwrap()).unwrap();
    engine.release_output(&mut rendered).unwrap();
    engine.release_output(&mut rendered).unwrap();
    assert!(engine.runtime().refresh_image(caller.key()).is_ok());
    check_readbacks(
        engine,
        rendered.submission.id(),
        &[(rendered.readback.unwrap(), caller_bytes)],
    );
    drop(rendered);

    let mut frame = engine.begin_graph().unwrap();
    let output = frame.graph.image("owned selected HDR", hdr(3, 2)).unwrap();
    let owned_bytes = half_pixels([0x4000, 0x4400, 0, 0x3c00], 6);
    frame
        .graph
        .upload_image("owned selected contents", output, &owned_bytes)
        .unwrap();
    let selected = frame.select_output(output, true).unwrap();
    assert_eq!(selected.output_ownership(), FrameOutputOwnership::Frame);
    let mut rendered = engine.submit_frame(selected.compile().unwrap()).unwrap();
    let original = rendered.output.key();
    rendered.output = engine.runtime().refresh_image(caller.key()).unwrap();
    engine.release_output(&mut rendered).unwrap();
    engine.release_output(&mut rendered).unwrap();
    assert!(engine.runtime().refresh_image(original).is_err());
    assert!(engine.runtime().refresh_image(caller.key()).is_ok());
    check_readbacks(
        engine,
        rendered.submission.id(),
        &[(rendered.readback.unwrap(), owned_bytes)],
    );
    drop(rendered);
    engine.runtime_mut().delete(caller.key()).unwrap();

    let held: [_; PREPARED_FRAMES] = std::array::from_fn(|_| engine.begin_graph().unwrap());
    let [first, second, third, fourth] = held;
    let compiled = first.compile().unwrap();
    assert!(matches!(
        engine.begin_graph(),
        Err(EngineRenderError::Preparation(
            MeshPreparationError::FrameCapacityExhausted
        ))
    ));
    drop((compiled, second, third, fourth));
    drop(
        engine
            .begin_graph()
            .expect("cancellation returns prepared capacity"),
    );
}

fn mesh_only_capacity(mode: ValidationMode) {
    let mut runtime = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 2,
            resource_capacity: 2,
            staging_bytes: 64 * 1024,
            ..RuntimeConfig::default()
        },
        mode,
    )
    .unwrap();
    let monitor = runtime.validation_monitor();
    runtime
        .prepare_working_set(
            FrameCapacity::default(),
            &[
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 112 },
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 24 },
                    count: 1,
                },
            ],
        )
        .unwrap();
    let mut engine = VulkanEngine::new(
        runtime,
        MeshCacheConfig {
            max_meshes: 1,
            max_bytes: 1024,
            max_upload_submissions: 2,
            max_retired_sources: 4,
        },
        EngineFrameCapacity::default(),
    )
    .unwrap();
    let mut assets = Engine::new(EngineCapacity {
        meshes: 1,
        materials: 1,
        retired_mesh_versions: 1,
    })
    .unwrap();
    let snapshots = SnapshotPool::new(1, 1).unwrap();
    let mesh = assets.create_mesh(quad([2.0, 0.0, 0.0, 1.0])).unwrap();
    let material = assets.create_material(Material::default()).unwrap();
    let view = View {
        width: 100,
        height: 100,
        clip_from_world: IDENTITY,
        clear_color: [0.0; 4],
    };
    let snapshot = assets
        .snapshot(
            &snapshots,
            &view,
            &[Draw {
                mesh,
                material,
                world_from_model: IDENTITY,
            }],
        )
        .unwrap();
    assert!(matches!(
        engine.prepare_snapshot(&snapshot, 1),
        Err(EngineRenderError::Preparation(
            MeshPreparationError::ResourceCapacityTooSmall {
                required_resources: 3,
                capacity: 2
            }
        ))
    ));
    assert!(matches!(
        engine.prepare_snapshot(&snapshot, usize::MAX),
        Err(EngineRenderError::Preparation(
            MeshPreparationError::ResourceCapacityTooSmall { .. }
        ))
    ));
    prepare(&mut engine, &snapshot);
    let mut frame = engine.begin_graph().unwrap();
    let inputs = engine.connect_snapshot(&mut frame, &snapshot).unwrap();
    let vertices = inputs.draws().next().unwrap().vertices;
    let readback = frame
        .graph
        .readback_buffer("mesh-only readback", vertices, 0, 16)
        .unwrap();
    drop(inputs);
    drop(snapshot);
    assets.delete_mesh(mesh).unwrap();
    assets.delete_material(material).unwrap();
    let plan = frame.compile().unwrap();
    engine.unload_mesh(mesh);
    engine.collect_retired_meshes(Some(&assets)).unwrap();
    assert_eq!(engine.mesh_cache_stats().prepared_retained_meshes, 1);
    let submitted = engine.submit_graph(plan).unwrap();
    let expected = [-1.0_f32, -1.0, 0.5, 2.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    check_readbacks(&mut engine, submitted.id(), &[(readback, expected)]);
    engine.collect_retired_meshes(Some(&assets)).unwrap();
    assert_eq!(engine.runtime().stats().registered_resources, 0);
    drop(engine);
    assert!(monitor.report().is_clean());
}

fn acceptance(strict: bool) {
    if cfg!(debug_assertions) {
        panic!("native acceptance requires --release");
    }
    let mode = if strict {
        ValidationMode::Required
    } else {
        ValidationMode::Disabled
    };
    mesh_only_capacity(mode);
    let mut runtime = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 4,
            resource_capacity: 16,
            staging_bytes: 64 * 1024,
            max_shader_bindings: 8,
            staging_ranges: 64,
        },
        mode,
    )
    .unwrap();
    assert_ne!(
        runtime.device_info().device_type,
        "CPU",
        "hardware GPU required"
    );
    let monitor = runtime.validation_monitor();
    runtime
        .prepare_working_set(
            FrameCapacity {
                graph: capacity(),
                range_snapshots: 64,
                ..FrameCapacity::default()
            },
            &[
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 16 },
                    count: 3,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 112 },
                    count: 2,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 24 },
                    count: 3,
                },
                ResourceClass::Image {
                    desc: hdr(6, 4),
                    count: 1,
                },
                ResourceClass::Image {
                    desc: hdr(3, 2),
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(5, 3),
                    count: 1,
                },
            ],
        )
        .unwrap();
    runtime
        .prepare_raster_programs(&[ManagedRasterProgram::mesh_hdr()])
        .unwrap();
    runtime
        .prepare_compute_programs(&[ManagedProgram::storage_readback()])
        .unwrap();
    let mut engine = VulkanEngine::new(
        runtime,
        MeshCacheConfig {
            max_meshes: 2,
            max_bytes: 1024,
            max_upload_submissions: 2,
            max_retired_sources: 4,
        },
        EngineFrameCapacity {
            prepared_frames: PREPARED_FRAMES,
            graph_slots: 8,
            graph: capacity(),
        },
    )
    .unwrap();
    without_snapshot(&mut engine);
    resident_versions(&mut engine);
    output_authority(&mut engine);
    assert_eq!(engine.runtime().pending_frames(), 0);
    assert_eq!(engine.runtime().stats().registered_resources, 0);
    drop(engine);
    let report = monitor.report();
    assert_eq!(report.requested, mode);
    if strict {
        assert_eq!(report.core_validation, Some(true), "{report:#?}");
        assert_eq!(report.synchronization_validation, Some(true), "{report:#?}");
    } else {
        if cfg!(any(
            feature = "development-validation",
            feature = "vulkan-diagnostics",
            feature = "graph-diagnostics"
        )) {
            panic!("shipping acceptance requires diagnostics disabled");
        }
    }
    assert!(
        report.is_clean(),
        "validation through native destruction: {report:#?}"
    );
}

#[cfg(feature = "development-validation")]
#[test]
#[ignore = "real GPU caller-composed resident-input acceptance; requires effective Core/SyncVal"]
fn caller_composed_engine_graph_strict() {
    acceptance(true);
}

#[cfg(not(any(
    feature = "development-validation",
    feature = "vulkan-diagnostics",
    feature = "graph-diagnostics"
)))]
#[test]
#[ignore = "real GPU caller-composed resident-input acceptance; shipping native validation disabled"]
fn caller_composed_engine_graph_shipping() {
    acceptance(false);
}

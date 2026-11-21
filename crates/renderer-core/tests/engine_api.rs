use renderer::unlit_forward::FrameSnapshot;
use renderer::unlit_forward::{Draw, IDENTITY, Vertex, View, mesh_data};
#[path = "../fixtures/image_transform.rs"]
mod image_transform;
use image_transform::Transform;

use renderer::unlit_forward::snapshot::build_graph;
use renderer_core::geometry::{MeshCapacity, MeshRegistry, SnapshotPool};
use renderer_core::graph::FrameOutputOwnership;
use renderer_core::graph::{GraphCapacity, GraphPool};

#[test]
fn owned_frame_inputs_need_no_gpu_or_window() {
    let mut engine = MeshRegistry::new(MeshCapacity {
        meshes: 2,
        retired_mesh_versions: 2,
    })
    .unwrap();
    let snapshots = SnapshotPool::new(1, 2).unwrap();
    let mut graphs = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mesh = engine
        .create_mesh(
            mesh_data(
                &[Vertex {
                    position: [0.0, 0.0, 0.5],
                    color: [1.0; 4],
                }],
                &[0, 0, 0],
            )
            .unwrap(),
        )
        .unwrap();
    let tint = [1.0; 4];
    let view = View {
        width: 32,
        height: 24,
        clip_from_world: IDENTITY,
        clear_color: [0.0, 0.0, 0.0, 1.0],
    };
    let draw = Draw {
        mesh,
        tint,
        world_from_model: IDENTITY,
    };
    let captured = FrameSnapshot::capture(&engine, &snapshots, &view, &[draw]).unwrap();
    engine.delete_mesh(mesh).unwrap();

    assert_eq!(captured.draw_count(), 1);
    assert_eq!(*captured.view(), view);
    let frame = build_graph(&captured, &mut graphs, true, |graph, color| {
        let desc = graph.image_desc(color)?;
        let transformed = graph.image("external custom output", desc)?;
        image_transform::dispatch(
            graph,
            "external custom transform",
            color,
            transformed,
            Transform::SwapRedBlue,
        )?;
        let unused = graph.image("unused custom output", desc)?;
        image_transform::dispatch(
            graph,
            "unused custom branch",
            color,
            unused,
            Transform::Invert,
        )?;
        Ok(transformed)
    })
    .unwrap();
    let output = frame.output();
    frame.readback().expect("requested final output readback");
    assert_eq!(
        frame.graph().image_desc(output).unwrap(),
        renderer_core::graph::ImageDesc::rgba8(view.width, view.height)
    );
    assert_eq!(frame.output_ownership(), FrameOutputOwnership::Frame);
    let plan = frame.compile().unwrap();
    assert!(
        plan.ordered_pass_names()
            .any(|name| name == "view readback")
    );
    assert!(
        plan.ordered_pass_names()
            .any(|name| name == "external custom transform")
    );
    assert!(
        !plan
            .ordered_pass_names()
            .any(|name| name == "unused custom branch")
    );
    #[cfg(feature = "graph-diagnostics")]
    assert!(
        plan.culled_pass_names()
            .any(|name| name == "unused custom branch")
    );
    assert!(FrameSnapshot::capture(&engine, &snapshots, &view, &[draw]).is_err());
    assert_eq!(
        FrameSnapshot::capture(&engine, &snapshots, &view, &[])
            .unwrap()
            .draw_count(),
        0
    );
}

#[test]
fn identity_extension_exports_its_input_without_a_copy() {
    let engine = MeshRegistry::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 0,
    })
    .unwrap();
    let snapshots = SnapshotPool::new(0, 1).unwrap();
    let mut graphs = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let view = View {
        width: 17,
        height: 11,
        clip_from_world: IDENTITY,
        clear_color: [0.0, 0.0, 0.0, 1.0],
    };
    let snapshot = FrameSnapshot::capture(&engine, &snapshots, &view, &[]).unwrap();
    let mut returned = None;
    let frame = build_graph(&snapshot, &mut graphs, false, |_, color| {
        returned = Some(color);
        Ok(color)
    })
    .unwrap();
    assert_eq!(frame.output(), returned.unwrap());
    assert_eq!(frame.output_ownership(), FrameOutputOwnership::Frame);
    let plan = frame.compile().unwrap();
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        ["view clear"]
    );
}

#[test]
fn caller_graph_connects_short_lived_inputs_without_selecting_passes_or_outputs() {
    use renderer_core::graph::{
        Attachment, BufferDesc, ImageDesc, LoadOp, RasterAttachments, StoreOp,
    };
    use renderer_core::program::ManagedRasterProgram;
    let mut engine = MeshRegistry::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 2,
    })
    .unwrap();
    let snapshots = SnapshotPool::new(2, 2).unwrap();
    let mut graphs = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = graphs.begin().unwrap();
    let data = mesh_data(
        &[Vertex {
            position: [0.0, 0.0, 0.5],
            color: [1.0; 4],
        }],
        &[0, 0, 0],
    )
    .unwrap();
    let mesh = engine.create_mesh(data.clone()).unwrap();
    let tint = [1.0; 4];
    let draw = Draw {
        mesh,
        tint,
        world_from_model: IDENTITY,
    };
    let first_view = View {
        width: 32,
        height: 24,
        clip_from_world: IDENTITY,
        clear_color: [0.0; 4],
    };
    let second_view = View {
        width: 10,
        height: 20,
        ..first_view
    };
    let first = FrameSnapshot::capture(&engine, &snapshots, &first_view, &[draw]).unwrap();
    engine.update_mesh(mesh, data).unwrap();
    let draw = Draw {
        tint: [0.5; 4],
        ..draw
    };
    let second = FrameSnapshot::capture(&engine, &snapshots, &second_view, &[draw, draw]).unwrap();
    let a = first.connect_graph(&mut graph).unwrap();
    let b = second.connect_graph(&mut graph).unwrap();
    assert_eq!(first.view(), &first_view);
    assert_eq!(second.view(), &second_view);
    assert_ne!(
        a.draws().next().unwrap().vertices,
        b.draws().next().unwrap().vertices
    );
    assert_eq!(
        first.connect_graph(&mut graph).err().unwrap().kind(),
        renderer_core::graph::GraphErrorKind::CapacityExhausted
    );
    let mut outputs = Vec::new();
    for (name, inputs) in [("first", &a), ("second", &b)] {
        let color = graph.image(name, ImageDesc::rgba16_float(7, 5)).unwrap();
        graph
            .raster(
                name,
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[Attachment {
                        view: graph.image_view(color, 0, 0).unwrap(),
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    depth: None,
                },
                |raster| raster.draw(&ManagedRasterProgram::mesh_hdr(), inputs.draws()),
            )
            .unwrap();
        outputs.push(color);
    }
    let source = a.draws().next().unwrap().vertices;
    assert!(
        graph
            .upload_buffer("immutable", source, 0, &[0; 4])
            .is_err()
    );
    assert!(graph.export_buffer(source).is_err());
    let copy = graph
        .buffer("independent copy", BufferDesc { size: 28 })
        .unwrap();
    graph
        .copy_buffer("copy input", source, 0, copy, 0, 28)
        .unwrap();
    graph.export_buffer(copy).unwrap();
    for output in outputs {
        graph.export_image(output).unwrap();
    }
    drop((a, b));
    drop((first, second));
    engine.delete_mesh(mesh).unwrap();

    let plan = graph.compile().unwrap();
    assert_eq!(plan.pass_count(), 7); // four mesh uploads, two selected Raster passes, one copy.
    assert!(
        !plan
            .ordered_pass_names()
            .any(|name| name.starts_with("view "))
    );
}

#[test]
fn unused_snapshot_connection_is_culled_and_capacity_failure_returns_scratch() {
    use renderer_core::graph::{BufferDesc, GraphCapacityKind, GraphErrorKind};
    let mut engine = MeshRegistry::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })
    .unwrap();
    let snapshots = SnapshotPool::new(1, 1).unwrap();
    let mesh = engine
        .create_mesh(
            mesh_data(
                &[Vertex {
                    position: [0.0; 3],
                    color: [1.0; 4],
                }],
                &[0, 0, 0],
            )
            .unwrap(),
        )
        .unwrap();
    let tint = [1.0; 4];
    let view = View {
        width: 8,
        height: 8,
        clip_from_world: IDENTITY,
        clear_color: [0.0; 4],
    };
    let snapshot = FrameSnapshot::capture(
        &engine,
        &snapshots,
        &view,
        &[Draw {
            mesh,
            tint,
            world_from_model: IDENTITY,
        }],
    )
    .unwrap();
    let mut small = GraphPool::new(
        GraphCapacity {
            resources: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = small.begin().unwrap();
    let error = snapshot.connect_graph(&mut graph).err().unwrap();
    assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
    assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Resources);
    drop(graph);
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    drop(snapshot.connect_graph(&mut graph).unwrap());
    let buffer = graph.buffer("only output", BufferDesc { size: 4 }).unwrap();
    graph
        .upload_buffer("user work", buffer, 0, &[1, 2, 3, 4])
        .unwrap();
    graph.export_buffer(buffer).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["user work"]
    );
}

use renderer::geometry::{GeometryData, MeshCapacity, MeshRegistry, SnapshotPool};
use renderer::graph::FrameOutputOwnership;
use renderer::graph::{
    GraphCapacity, GraphErrorKind, GraphPool, ImageDesc, ImageHandle, ResourceHandle,
};
use renderer_pipeline::unlit_forward::FrameSnapshot;
use renderer_pipeline::unlit_forward::snapshot::build_graph;
use renderer_pipeline::unlit_forward::{Draw, IDENTITY, Vertex, View, mesh_data};

fn graph_pool() -> GraphPool {
    GraphPool::new(
        GraphCapacity {
            resources: 512,
            passes: 512,
            ..GraphCapacity::default()
        },
        2,
    )
    .unwrap()
}

fn triangle() -> GeometryData {
    mesh_data(
        &[[-0.5, -0.5, 0.25], [0.5, -0.5, 0.25], [0.0, 0.5, 0.25]].map(|position| Vertex {
            position,
            color: [1.0; 4],
        }),
        &[0, 1, 2],
    )
    .unwrap()
}

fn view() -> View {
    View {
        width: 16,
        height: 16,
        clip_from_world: IDENTITY,
        clear_color: [0.0, 0.0, 0.0, 1.0],
    }
}

#[test]
fn opaque_pass_count_is_independent_of_repeated_draw_count() {
    let mut graph_pool = graph_pool();
    let pool = SnapshotPool::new(256, 1).unwrap();
    let mut assets = MeshRegistry::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })
    .unwrap();
    let mesh = assets.create_mesh(triangle()).unwrap();
    let tint = [1.0; 4];
    let draw = Draw {
        mesh,
        tint,
        world_from_model: IDENTITY,
    };
    for count in [1, 32, 256] {
        let snapshot = FrameSnapshot::capture(&assets, &pool, &view(), &vec![draw; count]).unwrap();
        let frame = build_graph(&snapshot, &mut graph_pool, false, |_, color| Ok(color)).unwrap();
        // Two shared mesh uploads and one opaque list; output is the list's color.
        assert_eq!(frame.compile().unwrap().pass_count(), 3);
    }
}

#[test]
fn opaque_pass_keeps_every_distinct_mesh_producer() {
    let mut graph_pool = graph_pool();
    let pool = SnapshotPool::new(256, 1).unwrap();
    const MESHES: usize = 128;
    let mut assets = MeshRegistry::new(MeshCapacity {
        meshes: MESHES,
        retired_mesh_versions: MESHES,
    })
    .unwrap();
    let tint = [1.0; 4];
    let draws = (0..MESHES)
        .map(|_| Draw {
            mesh: assets.create_mesh(triangle()).unwrap(),
            tint,
            world_from_model: IDENTITY,
        })
        .collect::<Vec<_>>();
    let snapshot = FrameSnapshot::capture(&assets, &pool, &view(), &draws).unwrap();
    let frame = build_graph(&snapshot, &mut graph_pool, false, |_, color| Ok(color)).unwrap();
    let plan = frame.compile().unwrap();
    assert_eq!(plan.pass_count(), MESHES * 2 + 1);
    #[cfg(feature = "graph-diagnostics")]
    assert!(plan.culled_pass_names().next().is_none());
}

#[test]
fn empty_view_has_initialized_output_without_uploads() {
    let mut graph_pool = graph_pool();
    let pool = SnapshotPool::new(256, 1).unwrap();
    let assets = MeshRegistry::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })
    .unwrap();
    let snapshot = FrameSnapshot::capture(
        &assets,
        &pool,
        &View {
            width: 4,
            height: 3,
            clip_from_world: IDENTITY,
            clear_color: [0.25, 0.5, 0.75, 1.0],
        },
        &[],
    )
    .unwrap();
    let frame = build_graph(&snapshot, &mut graph_pool, false, |_, color| Ok(color)).unwrap();
    assert_eq!(frame.output_ownership(), FrameOutputOwnership::Frame);
    assert!(frame.readback().is_none());
    let plan = frame.compile().unwrap();
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        ["view clear"]
    );
    assert_eq!(plan.pass_count(), 1);
}

#[test]
fn extension_output_checks_owner_format_extent_and_initialization() {
    let mut graph_pool = graph_pool();
    let pool = SnapshotPool::new(256, 1).unwrap();
    let assets = MeshRegistry::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })
    .unwrap();
    let snapshot = FrameSnapshot::capture(&assets, &pool, &view(), &[]).unwrap();
    let mut foreign_graph = graph_pool.begin().unwrap();
    let foreign = foreign_graph
        .image("foreign", ImageDesc::rgba8(16, 16))
        .unwrap();
    let error = match build_graph(&snapshot, &mut graph_pool, false, |_, _| Ok(foreign)) {
        Ok(_) => panic!("foreign output must fail during build"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
    assert_eq!(error.pass_name(), Some("view output"));
    assert_eq!(error.resource(), Some(foreign.into()));
    assert_eq!(error.resource_name(), None);

    let mut replacement_pool = self::graph_pool();
    let mut replacement = replacement_pool.begin().unwrap();
    let replacement_output = replacement
        .image("replacement output", ImageDesc::rgba8(16, 16))
        .unwrap();
    let error = build_graph(&snapshot, &mut graph_pool, false, |graph, _| {
        *graph = replacement;
        Ok(replacement_output)
    })
    .err()
    .expect("extension cannot replace the recipe's Graph");
    assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
    assert_eq!(error.pass_name(), Some("view output"));

    for (name, desc) in [
        ("wrong format", ImageDesc::depth32(16, 16)),
        ("wrong extent", ImageDesc::rgba8(8, 16)),
    ] {
        let error = match build_graph(&snapshot, &mut graph_pool, false, |graph, _| {
            graph.image(name, desc)
        }) {
            Ok(_) => panic!("incompatible output must fail during build"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some("view output"));
        assert_eq!(error.pass(), None);
        assert_eq!(error.resource_name(), Some(name));
        assert!(matches!(error.resource(), Some(ResourceHandle::Image(_))));
    }

    let mut uninitialized = None::<ImageHandle>;
    let frame = build_graph(&snapshot, &mut graph_pool, false, |graph, color| {
        let output = graph.image("uninitialized output", graph.image_desc(color)?)?;
        uninitialized = Some(output);
        Ok(output)
    })
    .unwrap();
    let error = frame.compile().unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
    assert_eq!(error.resource(), Some(uninitialized.unwrap().into()));
}

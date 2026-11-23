use renderer::{
    passes::{self, OpaqueDepth},
    unlit_forward::{self, ForwardConfig},
};
use renderer_core::graph::{
    Attachment, BufferDesc, Graph, GraphCapacity, GraphCapacityKind, GraphErrorKind, GraphPool,
    ImageDesc, ImageHandle, LoadOp, RasterAttachments, RasterDraw, RasterParameters, StoreOp,
};
use renderer_core::program::ManagedRasterProgram;

fn config(color: ImageDesc, depth: bool) -> ForwardConfig {
    ForwardConfig {
        color,
        clear_color: [0.1, 0.2, 0.3, 1.0],
        depth,
    }
}

fn mesh(graph: &mut Graph) -> RasterDraw {
    let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
    let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
    graph
        .upload_buffer("vertices", vertices, 0, &[0; 84])
        .unwrap();
    graph
        .upload_buffer("indices", indices, 0, &[0; 12])
        .unwrap();
    RasterDraw {
        secondary_vertices: None,
        vertices,
        indices,
        first_index: 0,
        index_count: 3,
        instance_count: 1,
        parameters: RasterParameters::default().into(),
        bindings: None,
    }
}

fn target(graph: &Graph, image: ImageHandle, load: LoadOp<[f32; 4]>) -> Attachment<[f32; 4]> {
    Attachment {
        view: graph.image_view(image, 0, 0).unwrap(),
        load,
        store: StoreOp::Store,
    }
}

#[test]
fn forward_can_be_used_directly_without_implicit_consumers_or_a_depth_target() {
    // One resource proves the color-only recipe does not allocate depth or a
    // redundant final-copy target. Each reused frame changes its configuration.
    let mut pool = GraphPool::new(
        GraphCapacity {
            resources: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    for (frame, desc) in [ImageDesc::rgba8(3, 2), ImageDesc::rgba16_float(7, 5)]
        .into_iter()
        .enumerate()
    {
        let mut graph = pool.begin().unwrap();
        let mut settings = config(desc, false);
        settings.clear_color[0] = frame as f32;
        let result = unlit_forward::forward(
            &mut graph,
            "empty forward",
            std::iter::empty::<RasterDraw>(),
            settings,
        )
        .unwrap();
        assert_eq!(graph.image_desc(result.color).unwrap(), desc);
        assert_eq!(result.depth, None);
        if frame == 0 {
            // Returning a Graph handle is not an export or a readback.
            assert_eq!(graph.compile().unwrap().pass_count(), 0);
        } else {
            graph.export_image(result.color).unwrap();
            assert_eq!(
                graph
                    .compile()
                    .unwrap()
                    .ordered_pass_names()
                    .collect::<Vec<_>>(),
                ["empty forward"]
            );
        }
    }
}

#[test]
fn forward_color_and_depth_can_feed_explicit_composition() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for desc in [ImageDesc::rgba8(4, 3), ImageDesc::rgba16_float(6, 2)] {
        let mut graph = pool.begin().unwrap();
        let draw = mesh(&mut graph);
        let settings = config(desc, true);
        let scene = unlit_forward::forward(&mut graph, "opaque", [draw], settings).unwrap();
        let depth = scene.depth.unwrap();
        assert_eq!(graph.image_desc(scene.color).unwrap(), desc);
        assert_eq!(
            graph.image_desc(depth).unwrap(),
            ImageDesc::depth32(desc.width, desc.height)
        );
        let middle = graph.image("post middle", desc).unwrap();
        let output = graph.image("post output", desc).unwrap();
        graph.copy_image("post first", scene.color, middle).unwrap();
        graph.copy_image("post second", middle, output).unwrap();
        graph.export_image(output).unwrap();
        graph.export_image(depth).unwrap();
        assert_eq!(
            graph
                .compile()
                .unwrap()
                .ordered_pass_names()
                .collect::<Vec<_>>(),
            ["vertices", "indices", "opaque", "post first", "post second"]
        );
    }
}

#[test]
fn standard_parts_compose_with_a_user_pass_without_the_standard_graph() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let draw = mesh(&mut graph);
    let color = graph.image("scene", ImageDesc::rgba16_float(4, 3)).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(4, 3)).unwrap();
    let depth_view = graph.image_view(depth, 0, 0).unwrap();
    passes::depth_prepass(
        &mut graph,
        "depth prepass",
        [draw],
        Attachment {
            view: depth_view,
            load: LoadOp::Clear(1.0),
            store: StoreOp::Store,
        },
    )
    .unwrap();
    let color_attachment = target(&graph, color, LoadOp::Clear([0.0; 4]));
    passes::opaque(
        &mut graph,
        "opaque after depth",
        [draw],
        color_attachment,
        Some(Attachment {
            view: depth_view,
            load: LoadOp::Load,
            store: StoreOp::Store,
        }),
        OpaqueDepth::Prepassed,
    )
    .unwrap();
    let color_attachment = target(&graph, color, LoadOp::Load);
    graph
        .raster(
            "user overlay",
            RasterAttachments {
                color_resolves: &[],
                colors: &[color_attachment],
                depth: None,
            },
            |raster| raster.draw(&ManagedRasterProgram::mesh_hdr(), [draw]),
        )
        .unwrap();
    let output = graph
        .image("output", ImageDesc::rgba16_float(4, 3))
        .unwrap();
    graph.copy_image("linear output", color, output).unwrap();
    graph.export_image(output).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        [
            "vertices",
            "indices",
            "depth prepass",
            "opaque after depth",
            "user overlay",
            "linear output"
        ]
    );
}

#[test]
fn prepassed_opaque_requires_loaded_depth_even_without_draws() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for has_draw in [false, true] {
        let mut graph = pool.begin().unwrap();
        let draw = mesh(&mut graph);
        let color = graph.image("color", ImageDesc::rgba8(4, 3)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(4, 3)).unwrap();
        let depth_view = graph.image_view(depth, 0, 0).unwrap();
        let color_attachment = target(&graph, color, LoadOp::Clear([0.0; 4]));
        for depth_attachment in [
            None,
            Some(Attachment {
                view: depth_view,
                load: LoadOp::Clear(1.0),
                store: StoreOp::Store,
            }),
        ] {
            let error = passes::opaque(
                &mut graph,
                "prepassed",
                std::iter::repeat_n(draw, usize::from(has_draw)),
                color_attachment,
                depth_attachment,
                OpaqueDepth::Prepassed,
            )
            .unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
            assert_eq!(error.pass_name(), Some("prepassed"));
        }
        passes::opaque(
            &mut graph,
            "uninitialized prepass input",
            std::iter::repeat_n(draw, usize::from(has_draw)),
            color_attachment,
            Some(Attachment {
                view: depth_view,
                load: LoadOp::Load,
                store: StoreOp::Store,
            }),
            OpaqueDepth::Prepassed,
        )
        .unwrap();
        assert_eq!(
            graph.compile().unwrap_err().kind(),
            GraphErrorKind::UninitializedRead
        );
    }
}

#[test]
fn load_and_discard_keep_the_ordinary_graph_initialization_contract() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for has_draw in [false, true] {
        let mut graph = pool.begin().unwrap();
        let draw = mesh(&mut graph);
        let color = graph
            .image("uninitialized", ImageDesc::rgba8(4, 3))
            .unwrap();
        let attachment = target(&graph, color, LoadOp::Load);
        passes::opaque(
            &mut graph,
            "load color",
            std::iter::repeat_n(draw, usize::from(has_draw)),
            attachment,
            None,
            OpaqueDepth::Write,
        )
        .unwrap();
        assert_eq!(
            graph.compile().unwrap_err().kind(),
            GraphErrorKind::UninitializedRead
        );
    }
    for discard in [false, true] {
        let mut graph = pool.begin().unwrap();
        let settings = config(ImageDesc::rgba8(4, 3), false);
        let scene = unlit_forward::forward(
            &mut graph,
            "scene",
            std::iter::empty::<RasterDraw>(),
            settings,
        )
        .unwrap();
        let output = if discard {
            let mut color = target(&graph, scene.color, LoadOp::Load);
            color.store = StoreOp::Discard;
            graph
                .attachment_pass(
                    "discard post output",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[color],
                        depth: None,
                    },
                )
                .unwrap();
            scene.color
        } else {
            graph
                .image("unwritten post output", settings.color)
                .unwrap()
        };
        graph.export_image(output).unwrap();
        assert_eq!(
            graph.compile().unwrap_err().kind(),
            GraphErrorKind::UninitializedRead
        );
    }
}

#[test]
fn unused_standard_graph_and_post_branches_are_culled_normally() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let settings = config(ImageDesc::rgba8(4, 3), true);
    unlit_forward::forward(
        &mut graph,
        "unused recipe",
        std::iter::empty::<RasterDraw>(),
        settings,
    )
    .unwrap();
    let scene = unlit_forward::forward(
        &mut graph,
        "selected recipe",
        std::iter::empty::<RasterDraw>(),
        settings,
    )
    .unwrap();
    let unused = graph.image("unused branch", settings.color).unwrap();
    graph
        .copy_image("unused effect", scene.color, unused)
        .unwrap();
    graph.export_image(scene.color).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["selected recipe"]
    );
}

#[test]
fn standard_parts_keep_capacity_errors_and_return_failed_draw_capacity() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            draws: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let draw = mesh(&mut graph);
    let color = graph.image("color", ImageDesc::rgba8(4, 3)).unwrap();
    let attachment = target(&graph, color, LoadOp::Clear([0.0; 4]));
    let error = passes::opaque(
        &mut graph,
        "overflow",
        [draw, draw],
        attachment,
        None,
        OpaqueDepth::Write,
    )
    .unwrap_err();
    assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Draws);
    passes::opaque(
        &mut graph,
        "retry",
        [draw],
        attachment,
        None,
        OpaqueDepth::Write,
    )
    .unwrap();
    graph.export_image(color).unwrap();
    assert_eq!(graph.compile().unwrap().pass_count(), 3);

    let mut tiny = GraphPool::new(
        GraphCapacity {
            resources: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = tiny.begin().unwrap();
    let error = unlit_forward::forward(
        &mut graph,
        "capacity",
        std::iter::empty::<RasterDraw>(),
        config(ImageDesc::rgba8(4, 3), true),
    )
    .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
    assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Resources);
}

#[test]
fn invalid_forward_configuration_is_rejected_before_declaring_passes() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for desc in [
        ImageDesc::depth32(4, 3),
        ImageDesc {
            mip_levels: 2,
            ..ImageDesc::rgba8(4, 3)
        },
        ImageDesc {
            array_layers: 2,
            ..ImageDesc::rgba8(4, 3)
        },
    ] {
        let mut graph = pool.begin().unwrap();
        assert_eq!(
            unlit_forward::forward(
                &mut graph,
                "bad config",
                std::iter::empty::<RasterDraw>(),
                config(desc, false)
            )
            .unwrap_err()
            .kind(),
            GraphErrorKind::Custom
        );
        assert_eq!(graph.compile().unwrap().pass_count(), 0);
    }
}

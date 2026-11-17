use renderer::graph::{
    Attachment, BufferDesc, Graph, GraphCapacity, GraphCapacityKind, GraphError, GraphErrorKind,
    GraphPool, ImageDesc, ImageHandle, LoadOp, RasterAttachments, RasterDraw, RasterParameters,
    StoreOp,
};
use renderer::program::ManagedRasterProgram;
use renderer::standard::{
    graphs::{self, ForwardConfig},
    passes::{self, OpaqueDepth},
};

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
        vertices,
        indices,
        index_count: 3,
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
        let result = graphs::forward(
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
fn multi_node_post_borrows_frame_data_and_preserves_hdr_and_optional_depth() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for desc in [ImageDesc::rgba8(4, 3), ImageDesc::rgba16_float(6, 2)] {
        let mut graph = pool.begin().unwrap();
        let draw = mesh(&mut graph);
        let mut calls = 0;
        let mut source = None;
        let mut post_output = None;
        let settings = config(desc, true);
        let result =
            graphs::forward_with_post(&mut graph, "opaque", [draw], settings, |graph, scene| {
                calls += 1;
                assert_eq!(graph.image_desc(scene.color)?, desc);
                assert_eq!(
                    graph.image_desc(scene.depth.unwrap())?,
                    ImageDesc::depth32(desc.width, desc.height)
                );
                source = Some(scene.color);
                let middle = graph.image("post middle", desc)?;
                let output = graph.image("post output", desc)?;
                graph.copy_image("post first", scene.color, middle)?;
                graph.copy_image("post second", middle, output)?;
                post_output = Some(output);
                Ok(output)
            })
            .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result.color, post_output.unwrap());
        assert_ne!(result.color, source.unwrap());
        assert!(result.depth.is_some());
        graph.export_image(result.color).unwrap();
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
            &ManagedRasterProgram::mesh_hdr(),
            [draw],
            RasterAttachments {
                colors: &[color_attachment],
                depth: None,
            },
        )
        .unwrap();
    let output = graph
        .image("output", ImageDesc::rgba16_float(4, 3))
        .unwrap();
    passes::output(&mut graph, "linear output", color, output).unwrap();
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
        let result = graphs::forward_with_post(
            &mut graph,
            "scene",
            std::iter::empty::<RasterDraw>(),
            settings,
            |graph, scene| {
                if discard {
                    let mut color = target(graph, scene.color, LoadOp::Load);
                    color.store = StoreOp::Discard;
                    graph.attachment_pass(
                        "discard post output",
                        RasterAttachments {
                            colors: &[color],
                            depth: None,
                        },
                    )?;
                    Ok(scene.color)
                } else {
                    graph.image("unwritten post output", settings.color)
                }
            },
        )
        .unwrap();
        graph.export_image(result.color).unwrap();
        assert_eq!(
            graph.compile().unwrap_err().kind(),
            GraphErrorKind::UninitializedRead
        );
    }
}

#[test]
fn post_output_contract_rejects_wrong_description_and_foreign_handles() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let settings = config(ImageDesc::rgba16_float(4, 3), false);
    for wrong in [ImageDesc::rgba16_float(3, 4), ImageDesc::rgba8(4, 3)] {
        let mut graph = pool.begin().unwrap();
        let error = graphs::forward_with_post(
            &mut graph,
            "scene",
            std::iter::empty::<RasterDraw>(),
            settings,
            |graph, _| graph.image("wrong output", wrong),
        )
        .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::Custom);
        assert!(error.to_string().contains("post output"));
    }
    let mut foreign_pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut foreign_graph = foreign_pool.begin().unwrap();
    let foreign = foreign_graph.image("foreign", settings.color).unwrap();
    let mut graph = pool.begin().unwrap();
    let error = graphs::forward_with_post(
        &mut graph,
        "scene",
        std::iter::empty::<RasterDraw>(),
        settings,
        |_, _| Ok(foreign),
    )
    .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
}

#[test]
fn post_cannot_replace_the_graph_and_return_a_matching_foreign_target() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut other_pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let mut other = other_pool.begin().unwrap();
    let settings = config(ImageDesc::rgba8(4, 3), true);
    let replacement = other.image("replacement output", settings.color).unwrap();
    let error = graphs::forward_with_post(
        &mut graph,
        "scene",
        std::iter::empty::<RasterDraw>(),
        settings,
        |graph, _| {
            drop(std::mem::replace(graph, other));
            Ok(replacement)
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
}

#[test]
fn unused_standard_graph_and_post_branches_are_culled_normally() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let settings = config(ImageDesc::rgba8(4, 3), true);
    graphs::forward(
        &mut graph,
        "unused recipe",
        std::iter::empty::<RasterDraw>(),
        settings,
    )
    .unwrap();
    let output = graphs::forward_with_post(
        &mut graph,
        "selected recipe",
        std::iter::empty::<RasterDraw>(),
        settings,
        |graph, scene| {
            let unused = graph.image("unused branch", settings.color)?;
            graph.copy_image("unused effect", scene.color, unused)?;
            Ok(scene.color)
        },
    )
    .unwrap();
    graph.export_image(output.color).unwrap();
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
    let error = graphs::forward(
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
fn recipe_configuration_and_callback_errors_remain_explicit() {
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
            graphs::forward(
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
    let mut graph = pool.begin().unwrap();
    let error = graphs::forward_with_post(
        &mut graph,
        "failed post",
        std::iter::empty::<RasterDraw>(),
        config(ImageDesc::rgba8(4, 3), false),
        |_, _| Err(GraphError::custom("post construction failed")),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "post construction failed");
}

use renderer::graph::{
    Access, AccessKind, AccessPurpose, AccessRange, Attachment, Graph, GraphCapacity,
    GraphCapacityKind, GraphErrorKind, GraphPool, ImageDesc, ImageView, LoadOp, PassHandle,
    PassKind, RasterAttachments, SampleCount, StoreOp,
};

fn color(view: ImageView, load: LoadOp<[f32; 4]>, store: StoreOp) -> Attachment<[f32; 4]> {
    Attachment { view, load, store }
}

fn clear(graph: &mut Graph, name: &str, view: ImageView) -> PassHandle {
    graph
        .attachment_pass(
            name,
            RasterAttachments {
                color_resolves: &[],
                colors: &[color(view, LoadOp::Clear([0.25; 4]), StoreOp::Store)],
                depth: None,
            },
        )
        .unwrap()
}

#[test]
fn msaa_resolve_keeps_a_distinct_single_sample_output_when_source_is_discarded() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let desc = ImageDesc::rgba16_float(8, 5);
    let msaa = graph
        .image("four samples", desc.with_sample_count(SampleCount::X4))
        .unwrap();
    let output = graph.image("one sample", desc).unwrap();
    let source_view = graph.image_view(msaa, 0, 0).unwrap();
    let output_view = graph.image_view(output, 0, 0).unwrap();
    assert_eq!(
        graph
            .readback_image("illegal raw readback", msaa)
            .unwrap_err()
            .kind(),
        GraphErrorKind::Unsupported
    );
    graph
        .attachment_pass(
            "clear and resolve",
            RasterAttachments {
                colors: &[color(
                    source_view,
                    LoadOp::Clear([1.0; 4]),
                    StoreOp::Discard,
                )],
                depth: None,
                color_resolves: &[Some(output_view)],
            },
        )
        .unwrap();
    graph.readback_image("resolved pixels", output).unwrap();
    let plan = graph.compile().unwrap();
    assert!(
        plan.ordered_pass_names()
            .eq(["clear and resolve", "resolved pixels"])
    );
}

#[test]
fn msaa_rejects_mixed_sample_counts_and_resolving_an_undefined_source() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let desc = ImageDesc::rgba8(4, 4);
    let msaa = graph
        .image("multisample color", desc.with_sample_count(SampleCount::X4))
        .unwrap();
    let output = graph.image("resolved color", desc).unwrap();
    let depth = graph
        .image("single-sample depth", ImageDesc::depth32(4, 4))
        .unwrap();
    let source_view = graph.image_view(msaa, 0, 0).unwrap();
    let resolve_view = graph.image_view(output, 0, 0).unwrap();
    let depth_view = graph.image_view(depth, 0, 0).unwrap();
    for (load, depth) in [
        (LoadOp::Discard, None),
        (
            LoadOp::Clear([0.0; 4]),
            Some(Attachment {
                view: depth_view,
                load: LoadOp::Clear(1.0),
                store: StoreOp::Discard,
            }),
        ),
    ] {
        let error = graph
            .attachment_pass(
                "invalid resolve",
                RasterAttachments {
                    colors: &[color(source_view, load, StoreOp::Discard)],
                    depth,
                    color_resolves: &[Some(resolve_view)],
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    }
    graph.readback_image("unwritten result", output).unwrap();
    assert!(graph.compile().is_err());
}

#[test]
fn subnormal_depth_clear_rejection_returns_capacity_for_zero_and_normal_minimum() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            attachments: 2,
            accesses: 4,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    for accepted in [0.0, f32::MIN_POSITIVE] {
        let mut graph = pool.begin().unwrap();
        let color_image = graph.image("color", ImageDesc::rgba8(2, 2)).unwrap();
        let color_view = graph.image_view(color_image, 0, 0).unwrap();
        let depth_image = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        let depth_view = graph.image_view(depth_image, 0, 0).unwrap();
        let colors = [color(color_view, LoadOp::Clear([0.25; 4]), StoreOp::Store)];
        let error = graph
            .attachment_pass(
                "subnormal depth",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &colors,
                    depth: Some(Attachment {
                        view: depth_view,
                        load: LoadOp::Clear(f32::MIN_POSITIVE / 2.0),
                        store: StoreOp::Store,
                    }),
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.resource(), Some(depth_image.into()));
        assert_eq!(error.pass_name(), Some("subnormal depth"));
        // The failed pass had already appended its color operand/access. A
        // successful retry at the exact prepared bounds proves both roll back.
        graph
            .attachment_pass(
                "accepted depth",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &colors,
                    depth: Some(Attachment {
                        view: depth_view,
                        load: LoadOp::Clear(accepted),
                        store: StoreOp::Store,
                    }),
                },
            )
            .unwrap();
        graph.readback_image("color result", color_image).unwrap();
        graph.readback_image("depth result", depth_image).unwrap();
        assert_eq!(
            graph
                .compile()
                .unwrap()
                .ordered_pass_names()
                .collect::<Vec<_>>(),
            ["accepted depth", "color result", "depth result"]
        );
    }
}

fn observe(graph: &mut Graph, name: &str, view: ImageView) {
    graph
        .declare_pass(
            name,
            PassKind::Copy,
            &[Access {
                resource: view.image().into(),
                range: AccessRange::Image {
                    mip: view.mip_level(),
                    layer: view.array_layer(),
                    aspect: view.aspect(),
                },
                kind: AccessKind::Read,
                purpose: AccessPurpose::CopySource,
            }],
            true,
        )
        .unwrap();
}

fn array_view(graph: &mut Graph, name: &str, mip: u32, layer: u32) -> ImageView {
    let image = graph
        .image(
            name,
            ImageDesc {
                mip_levels: 2,
                array_layers: 2,
                ..ImageDesc::rgba8(4, 4)
            },
        )
        .unwrap();
    graph.image_view(image, mip, layer).unwrap()
}

#[test]
fn mrt_and_optional_attachments_accept_matching_selected_mip_extents() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for (with_colors, with_depth) in [(true, true), (true, false), (false, true)] {
        let mut graph = pool.begin().unwrap();
        let a = array_view(&mut graph, "mipped color", 1, 1);
        let b = graph.image("base color", ImageDesc::rgba8(2, 2)).unwrap();
        let b = graph.image_view(b, 0, 0).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        let depth = graph.image_view(depth, 0, 0).unwrap();
        {
            let mut colors = [
                color(a, LoadOp::Clear([0.0, 0.25, 0.5, 1.0]), StoreOp::Store),
                color(b, LoadOp::Clear([1.0, 0.5, 0.25, 0.0]), StoreOp::Store),
            ];
            graph
                .attachment_pass(
                    "targets",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: if with_colors { &colors } else { &[] },
                        depth: with_depth.then_some(Attachment {
                            view: depth,
                            load: LoadOp::Clear(0.75),
                            store: StoreOp::Store,
                        }),
                    },
                )
                .unwrap();
            // The caller can overwrite and then drop the borrowed inputs before
            // compile. No alias of this array is retained by the plan.
            colors.fill(color(a, LoadOp::Discard, StoreOp::Discard));
            std::hint::black_box(colors);
        }
        if with_colors {
            observe(&mut graph, "color a", a);
            observe(&mut graph, "color b", b);
        }
        if with_depth {
            observe(&mut graph, "depth", depth);
        }
        let plan = graph.compile().unwrap();
        assert_eq!(
            plan.pass_count(),
            1 + 2 * usize::from(with_colors) + usize::from(with_depth)
        );
        assert_eq!(plan.ordered_pass_names().next(), Some("targets"));
    }
}

#[test]
fn clearing_one_cell_does_not_initialize_another_mip_or_layer() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for (mip, layer) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        let mut graph = pool.begin().unwrap();
        let selected = array_view(&mut graph, "array", 1, 1);
        clear(&mut graph, "selected clear", selected);
        let read = graph.image_view(selected.image(), mip, layer).unwrap();
        observe(&mut graph, "read cell", read);
        if (mip, layer) == (1, 1) {
            assert_eq!(graph.compile().unwrap().pass_count(), 2);
        } else {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(error.resource(), Some(selected.image().into()));
            assert_eq!(
                error.range(),
                Some(&AccessRange::Image {
                    mip,
                    layer,
                    aspect: read.aspect()
                })
            );
        }
    }
}

#[test]
fn whole_image_export_requires_all_mips_and_layers() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for complete in [false, true] {
        let mut graph = pool.begin().unwrap();
        let first = array_view(&mut graph, "array", 0, 0);
        clear(&mut graph, "base", first);
        if complete {
            for (mip, layer) in [(1, 0), (0, 1), (1, 1)] {
                let view = graph.image_view(first.image(), mip, layer).unwrap();
                clear(&mut graph, "other cell", view);
            }
        }
        graph.export_image(first.image()).unwrap();
        if complete {
            assert_eq!(graph.compile().unwrap().pass_count(), 4);
        } else {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(
                error.range(),
                Some(&AccessRange::Image {
                    mip: 1,
                    layer: 0,
                    aspect: first.aspect(),
                })
            );
        }
    }
}

#[test]
fn load_and_store_operations_preserve_or_invalidate_contents() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for (seed, load, store, valid) in [
        (false, LoadOp::Clear([0.0; 4]), StoreOp::Store, true),
        (true, LoadOp::Load, StoreOp::Store, true),
        (false, LoadOp::Load, StoreOp::Store, false),
        (true, LoadOp::Load, StoreOp::Discard, false),
        (true, LoadOp::Clear([0.0; 4]), StoreOp::Discard, false),
        (true, LoadOp::Discard, StoreOp::Store, false),
    ] {
        let mut graph = pool.begin().unwrap();
        let view = array_view(&mut graph, "array", 1, 1);
        if seed {
            clear(&mut graph, "seed", view);
        }
        graph
            .attachment_pass(
                "apply ops",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[color(view, load, store)],
                    depth: None,
                },
            )
            .unwrap();
        observe(&mut graph, "consumer", view);
        if valid {
            graph.compile().unwrap();
        } else {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(
                error.range(),
                Some(&AccessRange::Image {
                    mip: 1,
                    layer: 1,
                    aspect: view.aspect(),
                })
            );
        }
    }
}

#[test]
fn disjoint_layers_can_reorder_but_overlapping_load_dependencies_cannot() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for overlap in [false, true] {
        let mut graph = pool.begin().unwrap();
        let a = array_view(&mut graph, "array", 0, 0);
        let b = graph.image_view(a.image(), 0, u32::from(!overlap)).unwrap();
        let first = clear(&mut graph, "first", a);
        let second = if overlap {
            graph
                .attachment_pass(
                    "second",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[color(b, LoadOp::Load, StoreOp::Store)],
                        depth: None,
                    },
                )
                .unwrap()
        } else {
            clear(&mut graph, "second", b)
        };
        observe(&mut graph, "read a", a);
        observe(&mut graph, "read b", b);
        graph.depends_on(first, second).unwrap();
        if overlap {
            assert_eq!(
                graph.compile().unwrap_err().kind(),
                GraphErrorKind::DependencyCycle
            );
        } else {
            let plan = graph.compile().unwrap();
            let order = plan.ordered_pass_names().collect::<Vec<_>>();
            assert!(
                order.iter().position(|&name| name == "second").unwrap()
                    < order.iter().position(|&name| name == "first").unwrap()
            );
        }
    }
}

#[test]
fn culling_keeps_the_latest_producer_of_the_selected_view() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let view = array_view(&mut graph, "array", 0, 0);
    let unused = graph.image_view(view.image(), 0, 1).unwrap();
    clear(&mut graph, "old", view);
    clear(&mut graph, "unused layer", unused);
    clear(&mut graph, "current", view);
    observe(&mut graph, "result", view);
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["current", "result"]
    );
}

#[test]
fn invalid_target_inputs_leave_room_for_a_valid_pass() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            attachments: 2,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut foreign_pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut foreign_graph = foreign_pool.begin().unwrap();
    let foreign_image = foreign_graph
        .image("foreign", ImageDesc::rgba8(4, 4))
        .unwrap();
    let foreign = foreign_graph.image_view(foreign_image, 0, 0).unwrap();
    let stale = {
        let mut old = pool.begin().unwrap();
        let image = old.image("old", ImageDesc::rgba8(4, 4)).unwrap();
        old.image_view(image, 0, 0).unwrap()
    };
    let mut graph = pool.begin().unwrap();
    let image = graph.image("color", ImageDesc::rgba8(4, 4)).unwrap();
    let view = graph.image_view(image, 0, 0).unwrap();
    let small = graph.image("small", ImageDesc::rgba8(2, 2)).unwrap();
    let small = graph.image_view(small, 0, 0).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(4, 4)).unwrap();
    let depth = graph.image_view(depth, 0, 0).unwrap();
    for (name, second, kind) in [
        ("extent", small, GraphErrorKind::InvalidInput),
        ("format", depth, GraphErrorKind::InvalidInput),
        ("overlap", view, GraphErrorKind::InvalidInput),
        ("foreign", foreign, GraphErrorKind::WrongOwner),
        ("stale", stale, GraphErrorKind::WrongOwner),
    ] {
        let error = graph
            .attachment_pass(
                name,
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[
                        color(view, LoadOp::Clear([0.0; 4]), StoreOp::Store),
                        color(second, LoadOp::Clear([0.0; 4]), StoreOp::Store),
                    ],
                    depth: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), kind, "{name}");
        assert_eq!(error.pass_name(), Some(name));
    }
    assert_eq!(
        graph
            .attachment_pass(
                "empty",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[],
                    depth: None
                }
            )
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    for value in [f32::NAN, f32::INFINITY] {
        assert_eq!(
            graph
                .attachment_pass(
                    "bad color",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[color(view, LoadOp::Clear([value; 4]), StoreOp::Store)],
                        depth: None,
                    }
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
    }
    for value in [f32::NAN, f32::INFINITY, -0.25, 1.25] {
        assert_eq!(
            graph
                .attachment_pass(
                    "bad depth",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[],
                        depth: Some(Attachment {
                            view: depth,
                            load: LoadOp::Clear(value),
                            store: StoreOp::Store,
                        }),
                    }
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
    }
    assert_eq!(
        graph
            .attachment_pass(
                "color as depth",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[],
                    depth: Some(Attachment {
                        view,
                        load: LoadOp::Clear(1.0),
                        store: StoreOp::Store
                    }),
                }
            )
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    clear(&mut graph, "valid", view);
    graph.export_image(image).unwrap();
    assert_eq!(graph.compile().unwrap().pass_count(), 1);
}

#[test]
fn late_capacity_failures_roll_back_owned_attachments_and_accesses() {
    for (capacity, expected) in [
        (
            GraphCapacity {
                attachments: 1,
                ..GraphCapacity::default()
            },
            GraphCapacityKind::Attachments,
        ),
        (
            GraphCapacity {
                attachments: 2,
                accesses: 1,
                ..GraphCapacity::default()
            },
            GraphCapacityKind::Accesses,
        ),
        (
            GraphCapacity {
                attachments: 2,
                accesses: 2,
                name_bytes: 4,
                ..GraphCapacity::default()
            },
            GraphCapacityKind::NameBytes,
        ),
    ] {
        let mut pool = GraphPool::new(capacity, 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let image = graph.image("c", ImageDesc::rgba8(2, 2)).unwrap();
        let view = graph.image_view(image, 0, 0).unwrap();
        let depth = graph.image("d", ImageDesc::depth32(2, 2)).unwrap();
        let depth = graph.image_view(depth, 0, 0).unwrap();
        let error = graph
            .attachment_pass(
                "too long",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[color(view, LoadOp::Clear([0.0; 4]), StoreOp::Store)],
                    depth: Some(Attachment {
                        view: depth,
                        load: LoadOp::Clear(1.0),
                        store: StoreOp::Store,
                    }),
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
        assert_eq!(error.capacity().unwrap().kind, expected);
        // A single target fits every tested capacity; failure must not leave
        // partial accesses or owned operands occupying that prepared storage.
        clear(&mut graph, "x", view);
        graph.export_image(image).unwrap();
        assert_eq!(graph.compile().unwrap().pass_count(), 1);
    }
}

use renderer::ImageFormat;
use renderer::graph::{
    Access, AccessKind, AccessPurpose, AccessRange, Attachment, BufferDesc, Graph, GraphCapacity,
    GraphCapacityKind, GraphErrorKind, GraphPool, ImageDesc, ImageView, LoadOp, PassKind,
    RasterAttachments, RasterConstants, RasterDraw, RasterParameters, StoreOp,
};
use renderer::program::{ImageDescriptor, ManagedRasterProgram};

fn mesh(graph: &mut Graph, name: &str, seed_vertices: bool, seed_indices: bool) -> RasterDraw {
    let vertices = graph
        .buffer(&format!("{name} vertices"), BufferDesc { size: 84 })
        .unwrap();
    let indices = graph
        .buffer(&format!("{name} indices"), BufferDesc { size: 12 })
        .unwrap();
    if seed_vertices {
        graph
            .upload_buffer(&format!("{name} vertices"), vertices, 0, &[0; 84])
            .unwrap();
    }
    if seed_indices {
        graph
            .upload_buffer(&format!("{name} indices"), indices, 0, &[0; 12])
            .unwrap();
    }
    RasterDraw {
        bindings: None,
        vertices,
        indices,
        index_count: 3,
        parameters: RasterParameters::default().into(),
    }
}

fn color(graph: &mut Graph, name: &str) -> Attachment<[f32; 4]> {
    let image = graph.image(name, ImageDesc::rgba8(2, 2)).unwrap();
    Attachment {
        view: graph.image_view(image, 0, 0).unwrap(),
        load: LoadOp::Clear([0.25; 4]),
        store: StoreOp::Store,
    }
}

#[test]
fn shared_image_format_keeps_output_checks_and_transactional_capacity() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            draws: 1,
            attachments: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let draw = mesh(&mut graph, "mesh", true, true);
    let unorm = color(&mut graph, "unorm");
    let program = ManagedRasterProgram::mesh_hdr();
    // One format value flows from the shader contract through managed resource
    // metadata into a logical image, without a conversion or a second enum.
    let descriptor = ImageDescriptor {
        mip_levels: 1,
        width: 2,
        height: 2,
        format: program.color_outputs()[0],
        preferred_depth_clear: None,
    };
    let hdr_desc = ImageDesc {
        width: descriptor.width,
        height: descriptor.height,
        format: descriptor.format,
        ..ImageDesc::rgba8(2, 2)
    };
    assert_eq!(hdr_desc.format, ImageFormat::Rgba16Float);
    let hdr = graph.image("hdr", hdr_desc).unwrap();
    let hdr = Attachment {
        view: graph.image_view(hdr, 0, 0).unwrap(),
        ..unorm
    };
    for (program, target) in [
        (ManagedRasterProgram::mesh_hdr(), unorm),
        (ManagedRasterProgram::mesh(), hdr),
    ] {
        let error = graph
            .raster(
                "mismatched color",
                &program,
                [draw],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.resource(), Some(target.view.image().into()));
    }
    graph
        .raster(
            "hdr draw",
            &ManagedRasterProgram::mesh_hdr(),
            [draw],
            RasterAttachments {
                color_resolves: &[],
                colors: &[hdr],
                depth: None,
            },
        )
        .unwrap();
    graph
        .readback_image("hdr pixels", hdr.view.image())
        .unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["mesh vertices", "mesh indices", "hdr draw", "hdr pixels"]
    );
}

#[test]
fn whole_image_transfers_use_format_bytes_and_read_back_stored_depth() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let hdr = graph.image("hdr", ImageDesc::rgba16_float(2, 2)).unwrap();
    assert_eq!(
        graph.image_view(hdr, 0, 0).unwrap().aspect(),
        renderer::graph::ImageAspect::Color
    );
    assert_eq!(
        graph
            .upload_image("short HDR upload", hdr, &[0; 16])
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    graph.upload_image("HDR upload", hdr, &[0; 32]).unwrap();
    graph.readback_image("HDR result", hdr).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
    let view = graph.image_view(depth, 0, 0).unwrap();
    assert_eq!(view.aspect(), renderer::graph::ImageAspect::Depth);
    assert_eq!(
        graph
            .upload_image("unsupported depth upload", depth, &[0; 16])
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    graph
        .attachment_pass(
            "depth clear",
            RasterAttachments {
                color_resolves: &[],
                colors: &[],
                depth: Some(Attachment {
                    view,
                    load: LoadOp::Clear(0.5),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    graph.readback_image("depth result", depth).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["HDR upload", "HDR result", "depth clear", "depth result"]
    );
}

#[test]
fn depth_only_draws_require_depth_and_do_not_restore_discarded_contents() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for store in [StoreOp::Store, StoreOp::Discard] {
        let mut graph = pool.begin().unwrap();
        let draw = mesh(&mut graph, "mesh", true, true);
        let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        assert_eq!(
            graph
                .raster(
                    "missing depth",
                    &ManagedRasterProgram::depth_only(),
                    [draw],
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[],
                        depth: None
                    },
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
        graph
            .raster(
                "depth draw",
                &ManagedRasterProgram::depth_only(),
                [draw],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[],
                    depth: Some(Attachment {
                        view: graph.image_view(depth, 0, 0).unwrap(),
                        load: LoadOp::Clear(1.0),
                        store,
                    }),
                },
            )
            .unwrap();
        graph.readback_image("depth read", depth).unwrap();
        if store == StoreOp::Store {
            assert_eq!(graph.compile().unwrap().pass_count(), 4);
        } else {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(error.resource(), Some(depth.into()));
        }
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

#[test]
fn short_lived_draws_keep_both_meshes_alive_and_pin_the_completed_plan() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let a = mesh(&mut graph, "a", true, true);
    let b = mesh(&mut graph, "b", true, true);
    let target = color(&mut graph, "color");
    {
        let mut draws = [a, b];
        let mut targets = [target];
        graph
            .raster(
                "draws",
                &ManagedRasterProgram::mesh(),
                draws,
                RasterAttachments {
                    color_resolves: &[],
                    colors: &targets,
                    depth: None,
                },
            )
            .unwrap();
        draws.fill(RasterDraw {
            bindings: None,
            index_count: 0,
            ..a
        });
        targets[0].load = LoadOp::Discard;
        targets[0].store = StoreOp::Discard;
        std::hint::black_box((draws, targets));
    }
    observe(&mut graph, "pixels", target.view);
    let plan = graph.compile().unwrap();
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        [
            "a vertices",
            "a indices",
            "b vertices",
            "b indices",
            "draws",
            "pixels"
        ]
    );
    let retained = plan.clone();
    drop(plan);
    assert_eq!(
        pool.begin().unwrap_err().kind(),
        GraphErrorKind::CapacityExhausted
    );
    drop(retained);
    assert_eq!(pool.begin().unwrap().compile().unwrap().pass_count(), 0);
}

#[test]
fn a_later_draw_cannot_hide_an_uninitialized_vertex_or_index_input() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for missing_vertices in [true, false] {
        let mut graph = pool.begin().unwrap();
        let first = mesh(&mut graph, "ready", true, true);
        let later = mesh(&mut graph, "later", !missing_vertices, missing_vertices);
        let target = color(&mut graph, "color");
        let pass = graph
            .raster(
                "draw sequence",
                &ManagedRasterProgram::mesh(),
                [first, later],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None,
                },
            )
            .unwrap();
        observe(&mut graph, "pixels", target.view);
        let error = graph.compile().unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
        assert_eq!(error.pass(), Some(pass));
        assert_eq!(
            error.resource(),
            Some(
                if missing_vertices {
                    later.vertices
                } else {
                    later.indices
                }
                .into()
            )
        );
    }
}

#[test]
fn shared_vertex_index_inputs_keep_the_latest_producer_and_cull_older_contents() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let shared = graph
        .buffer("packed input", BufferDesc { size: 84 })
        .unwrap();
    graph
        .upload_buffer("overwritten", shared, 0, &[1; 84])
        .unwrap();
    graph.upload_buffer("current", shared, 0, &[0; 84]).unwrap();
    let draw = RasterDraw {
        bindings: None,
        vertices: shared,
        indices: shared,
        index_count: 3,
        parameters: RasterParameters::default().into(),
    };
    let target = color(&mut graph, "color");
    graph
        .raster(
            "shared draws",
            &ManagedRasterProgram::mesh(),
            [
                draw,
                RasterDraw {
                    bindings: None,
                    index_count: 6,
                    ..draw
                },
            ],
            RasterAttachments {
                color_resolves: &[],
                colors: &[target],
                depth: None,
            },
        )
        .unwrap();
    observe(&mut graph, "pixels", target.view);
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["current", "shared draws", "pixels"]
    );
}

#[test]
fn late_draw_and_attachment_capacity_failures_leave_room_for_a_valid_sequence() {
    for exhausted in [GraphCapacityKind::Attachments, GraphCapacityKind::Accesses] {
        let mut pool = GraphPool::new(
            GraphCapacity {
                draws: 2,
                attachments: if exhausted == GraphCapacityKind::Attachments {
                    1
                } else {
                    2
                },
                accesses: if exhausted == GraphCapacityKind::Accesses {
                    7
                } else {
                    GraphCapacity::default().accesses
                },
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let a = mesh(&mut graph, "a", true, true);
        let b = mesh(&mut graph, "b", true, true);
        let target = color(&mut graph, "color");
        let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        let depth = Attachment {
            view: graph.image_view(depth, 0, 0).unwrap(),
            load: LoadOp::Clear(1.0),
            store: StoreOp::Store,
        };
        let error = graph
            .raster(
                "too many operands",
                &ManagedRasterProgram::mesh(),
                [a, b],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: Some(depth),
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
        assert_eq!(error.capacity().unwrap().kind, exhausted);
        graph
            .raster(
                "valid",
                &ManagedRasterProgram::mesh(),
                [b],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None,
                },
            )
            .unwrap();
        graph.export_image(target.view.image()).unwrap();
        assert_eq!(
            graph
                .compile()
                .unwrap()
                .ordered_pass_names()
                .collect::<Vec<_>>(),
            ["b vertices", "b indices", "valid"],
            "failed draws must not keep unrelated inputs live"
        );
    }
}

#[test]
fn later_raster_passes_and_recycled_slots_do_not_keep_previous_draw_inputs() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for _ in 0..2 {
        let mut graph = pool.begin().unwrap();
        for _ in 0..24 {
            graph.buffer("unused", BufferDesc { size: 4 }).unwrap();
        }
        let old = mesh(&mut graph, "old", true, true);
        let current = mesh(&mut graph, "current", true, true);
        let target = color(&mut graph, "color");
        for (name, draw) in [("old draw", old), ("current draw", current)] {
            graph
                .raster(
                    name,
                    &ManagedRasterProgram::mesh(),
                    [draw],
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[target],
                        depth: None,
                    },
                )
                .unwrap();
        }
        observe(&mut graph, "pixels", target.view);
        assert_eq!(
            graph
                .compile()
                .unwrap()
                .ordered_pass_names()
                .collect::<Vec<_>>(),
            [
                "current vertices",
                "current indices",
                "current draw",
                "pixels"
            ]
        );
    }
}

#[test]
fn empty_raster_checks_program_outputs_and_clear_only_uses_attachment_pass() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for depth_only in [true, false] {
        let mut graph = pool.begin().unwrap();
        let draw = mesh(&mut graph, "unused mesh", false, false);
        let colors = [color(&mut graph, "a"), color(&mut graph, "b")];
        let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        let depth = Attachment {
            view: graph.image_view(depth, 0, 0).unwrap(),
            load: LoadOp::Clear(1.0),
            store: StoreOp::Store,
        };
        let targets = RasterAttachments {
            color_resolves: &[],
            colors: if depth_only { &[] } else { &colors },
            depth: depth_only.then_some(depth),
        };
        assert_eq!(
            graph
                .raster(
                    "unsupported draw outputs",
                    &ManagedRasterProgram::mesh(),
                    [draw],
                    targets
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::Unsupported
        );
        assert_eq!(
            graph
                .raster(
                    "empty draw list",
                    &ManagedRasterProgram::mesh(),
                    std::iter::empty::<RasterDraw>(),
                    targets,
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::Unsupported
        );
        graph.attachment_pass("clear only", targets).unwrap();
        observe(
            &mut graph,
            "pixels",
            if depth_only {
                depth.view
            } else {
                colors[0].view
            },
        );
        assert_eq!(
            graph
                .compile()
                .unwrap()
                .ordered_pass_names()
                .collect::<Vec<_>>(),
            ["clear only", "pixels"]
        );
    }
}

#[test]
fn color_only_drawing_initializes_only_its_selected_mip_and_layer() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for read_selected in [true, false] {
        let mut graph = pool.begin().unwrap();
        let draw = mesh(&mut graph, "mesh", true, true);
        let image = graph
            .image(
                "mipped color",
                ImageDesc {
                    mip_levels: 2,
                    array_layers: 2,
                    ..ImageDesc::rgba8(4, 4)
                },
            )
            .unwrap();
        let view = graph.image_view(image, 1, 1).unwrap();
        graph
            .raster(
                "selected target",
                &ManagedRasterProgram::mesh(),
                [draw],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[Attachment {
                        view,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    depth: None,
                },
            )
            .unwrap();
        let selected = graph
            .image_view(image, 1, if read_selected { 1 } else { 0 })
            .unwrap();
        observe(&mut graph, "pixels", selected);
        if read_selected {
            assert_eq!(graph.compile().unwrap().pass_count(), 4);
        } else {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(
                error.range(),
                Some(&AccessRange::Image {
                    mip: 1,
                    layer: 0,
                    aspect: selected.aspect()
                })
            );
        }
    }
}

#[test]
fn drawing_does_not_turn_discarded_or_missing_attachment_contents_into_full_initialization() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for (load, store, seed, expected_error) in [
        (LoadOp::Clear([0.0; 4]), StoreOp::Store, false, None),
        (LoadOp::Load, StoreOp::Store, true, None),
        (LoadOp::Load, StoreOp::Store, false, Some("draw")),
        (LoadOp::Discard, StoreOp::Store, true, Some("pixels")),
        (
            LoadOp::Clear([0.0; 4]),
            StoreOp::Discard,
            false,
            Some("pixels"),
        ),
        (LoadOp::Load, StoreOp::Discard, true, Some("pixels")),
    ] {
        let mut graph = pool.begin().unwrap();
        let draw = mesh(&mut graph, "mesh", true, true);
        let mut target = color(&mut graph, "color");
        if seed {
            graph
                .attachment_pass(
                    "seed",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[target],
                        depth: None,
                    },
                )
                .unwrap();
        }
        target.load = load;
        target.store = store;
        graph
            .raster(
                "draw",
                &ManagedRasterProgram::mesh(),
                [draw],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None,
                },
            )
            .unwrap();
        observe(&mut graph, "pixels", target.view);
        if let Some(pass_name) = expected_error {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(error.pass_name(), Some(pass_name));
        } else {
            assert!(graph.compile().is_ok());
        }
    }
}

#[test]
fn public_draw_validation_rejects_invalid_buffer_ranges() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            draws: 1,
            attachments: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let valid = mesh(&mut graph, "mesh", true, true);
    let target = color(&mut graph, "color");
    let bad_stride = graph.buffer("bad stride", BufferDesc { size: 85 }).unwrap();
    for invalid in [
        RasterDraw {
            bindings: None,
            index_count: 0,
            ..valid
        },
        RasterDraw {
            bindings: None,
            index_count: 6,
            ..valid
        },
        RasterDraw {
            bindings: None,
            vertices: bad_stride,
            ..valid
        },
    ] {
        let error = graph
            .raster(
                "invalid draw",
                &ManagedRasterProgram::mesh(),
                [invalid],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some("invalid draw"));
    }
    let mut other_pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut other = other_pool.begin().unwrap();
    let foreign = mesh(&mut other, "foreign", false, false);
    assert_eq!(
        graph
            .raster(
                "foreign draw",
                &ManagedRasterProgram::mesh(),
                [foreign],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None
                }
            )
            .unwrap_err()
            .kind(),
        GraphErrorKind::WrongOwner
    );
    graph
        .raster(
            "valid draw",
            &ManagedRasterProgram::mesh(),
            [valid],
            RasterAttachments {
                color_resolves: &[],
                colors: &[target],
                depth: None,
            },
        )
        .unwrap();
    observe(&mut graph, "pixels", target.view);
    assert_eq!(graph.compile().unwrap().pass_count(), 4);
}

#[test]
fn raster_parameter_creation_rejects_nonfinite_values_and_owns_constants() {
    let defaults = RasterParameters::default();
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut transform = *defaults.transform();
        transform[1][2] = value;
        assert_eq!(
            RasterParameters::new(transform, *defaults.tint())
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput,
        );
        let mut tint = *defaults.tint();
        tint[3] = value;
        assert_eq!(
            RasterParameters::new(*defaults.transform(), tint)
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput,
        );
    }
    let mut transform = *defaults.transform();
    transform[3][0] = 0.75;
    let mut tint = [0.25, 0.5, 0.75, 1.0];
    let parameters = RasterParameters::new(transform, tint).unwrap();
    transform[3][0] = f32::NAN;
    tint[3] = f32::INFINITY;
    std::hint::black_box((transform, tint));
    assert_eq!(parameters.transform()[3][0], 0.75);
    assert_eq!(parameters.tint(), &[0.25, 0.5, 0.75, 1.0]);
}

struct MisreportedDraws<I> {
    inner: I,
    reported: usize,
}

impl<I: Iterator> Iterator for MisreportedDraws<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.reported, Some(self.reported))
    }
}

impl<I: Iterator> ExactSizeIterator for MisreportedDraws<I> {
    fn len(&self) -> usize {
        self.reported
    }
}

#[test]
fn raster_uses_actual_iterator_items_for_empty_paths_and_capacity() {
    for (count, reported) in [(0, usize::MAX), (1, 0), (1, usize::MAX), (2, 0)] {
        let mut pool = GraphPool::new(
            GraphCapacity {
                draws: 1,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let draw = mesh(&mut graph, "mesh", true, true);
        let target = color(&mut graph, "color");
        let attachments = RasterAttachments {
            color_resolves: &[],
            colors: &[target],
            depth: None,
        };
        let result = graph.raster(
            "iterator",
            &ManagedRasterProgram::mesh(),
            MisreportedDraws {
                inner: std::iter::repeat_n(draw, count),
                reported,
            },
            attachments,
        );
        if count > 1 {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
            assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Draws);
            // A failed iterator capture returns its draw and attachment capacity.
            graph
                .raster(
                    "retry",
                    &ManagedRasterProgram::mesh(),
                    std::iter::once(draw),
                    attachments,
                )
                .unwrap();
        } else {
            result.unwrap();
        }
        observe(&mut graph, "pixels", target.view);
        assert_eq!(
            graph.compile().unwrap().pass_count(),
            if count == 0 { 2 } else { 4 }
        );
    }
}

#[test]
fn generic_constants_own_every_word_and_reject_oversized_payloads() {
    let mut words = [0x7fc0_1234, u32::MAX, 0, 0x8000_0000];
    let constants = RasterConstants::new(&words).unwrap();
    words.fill(7);
    assert_eq!(constants.words(), &[0x7fc0_1234, u32::MAX, 0, 0x8000_0000]);
    assert!(RasterConstants::default().words().is_empty());
    assert_eq!(RasterConstants::new(&[3; 32]).unwrap().words(), &[3; 32]);
    assert_eq!(
        RasterConstants::new(&[0; 33]).unwrap_err().kind(),
        GraphErrorKind::InvalidInput
    );
    let mesh: RasterConstants = RasterParameters::default().into();
    assert_eq!(mesh.words().len(), 20);
    for index in 0..16 {
        assert_eq!(
            mesh.words()[index],
            if index % 5 == 0 { 1.0_f32.to_bits() } else { 0 }
        );
    }
    assert_eq!(&mesh.words()[16..], &[1.0_f32.to_bits(); 4]);
}

#[test]
fn wrong_constant_length_returns_draw_capacity_for_a_correct_retry() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            draws: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let draw = mesh(&mut graph, "mesh", true, true);
    let target = color(&mut graph, "color");
    for length in [0, 19, 21] {
        let wrong = RasterDraw {
            parameters: RasterConstants::new(&[0; 32][..length]).unwrap(),
            ..draw
        };
        let error = graph
            .raster(
                "wrong inline ABI",
                &ManagedRasterProgram::mesh(),
                [wrong],
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some("wrong inline ABI"));
    }
    graph
        .raster(
            "correct inline ABI",
            &ManagedRasterProgram::mesh(),
            [draw],
            RasterAttachments {
                color_resolves: &[],
                colors: &[target],
                depth: None,
            },
        )
        .unwrap();
    observe(&mut graph, "pixels", target.view);
    graph.compile().unwrap();
}

#[test]
fn empty_draws_enforce_custom_depth_and_output_contracts() {
    use renderer::program::{CompareOp, DepthState, RasterProgramDescriptor};
    static REQUIRED: RasterProgramDescriptor = RasterProgramDescriptor {
        depth: DepthState::Required {
            compare: CompareOp::Greater,
            write: false,
            require_load: false,
        },
        ..*ManagedRasterProgram::mesh().descriptor()
    };
    // SAFETY: The unchanged reviewed mesh binaries and ABI support this fixed
    // depth state; there are no new resources, shader accesses or capabilities.
    let program = unsafe { ManagedRasterProgram::from_trusted_descriptor(&REQUIRED) };
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let target = color(&mut graph, "color");
    let empty = std::iter::empty::<RasterDraw>;
    assert_eq!(
        graph
            .raster(
                "missing required depth",
                &program,
                empty(),
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None
                }
            )
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
    let depth = Attachment {
        view: graph.image_view(depth, 0, 0).unwrap(),
        load: LoadOp::Clear(0.0),
        store: StoreOp::Store,
    };
    let hdr = graph
        .image("wrong format", ImageDesc::rgba16_float(2, 2))
        .unwrap();
    let wrong = Attachment {
        view: graph.image_view(hdr, 0, 0).unwrap(),
        ..target
    };
    assert_eq!(
        graph
            .raster(
                "wrong empty format",
                &program,
                empty(),
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[wrong],
                    depth: Some(depth)
                }
            )
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    graph
        .raster(
            "empty with all required targets",
            &program,
            empty(),
            RasterAttachments {
                color_resolves: &[],
                colors: &[target],
                depth: Some(depth),
            },
        )
        .unwrap();
    observe(&mut graph, "pixels", target.view);
    graph.compile().unwrap();
}

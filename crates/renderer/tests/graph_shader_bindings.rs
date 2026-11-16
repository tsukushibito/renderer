use renderer::graph::{
    Attachment, Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphCapacity,
    GraphCapacityKind, GraphErrorKind, GraphPool, ImageDesc, ImageView, LoadOp, RasterAttachments,
    RasterDraw, RasterParameters, StoreOp,
};
use renderer::program::{ManagedProgram, ManagedRasterProgram, SamplerDescriptor};

fn geometry(graph: &mut Graph) -> RasterDraw<'static> {
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
        bindings: &[],
    }
}

fn uniform(graph: &mut Graph) -> BufferHandle {
    let buffer = graph.buffer("offset", BufferDesc { size: 48 }).unwrap();
    graph
        .upload_buffer("offset bytes", buffer, 16, &[0; 16])
        .unwrap();
    buffer
}

fn bindings(uniform: BufferHandle, view: ImageView) -> [Binding; 3] {
    [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::Buffer {
                buffer: uniform,
                offset: 16,
                size: 32,
            },
        },
        Binding {
            group: 1,
            binding: 0,
            resource: BindingResource::ImageView(view),
        },
        Binding {
            group: 1,
            binding: 1,
            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
        },
    ]
}

fn color(graph: &mut Graph, name: &str) -> ImageView {
    let image = graph.image(name, ImageDesc::rgba8(2, 2)).unwrap();
    graph.image_view(image, 0, 0).unwrap()
}

fn attachment(view: ImageView) -> Attachment<[f32; 4]> {
    Attachment {
        view,
        load: LoadOp::Clear([0.0; 4]),
        store: StoreOp::Store,
    }
}

fn seed(graph: &mut Graph, name: &str, view: ImageView) {
    graph
        .attachment_pass(
            name,
            RasterAttachments {
                colors: &[attachment(view)],
                depth: None,
            },
        )
        .unwrap();
}

fn draw(
    graph: &mut Graph,
    name: &str,
    geometry: RasterDraw<'_>,
    bindings: &[Binding],
    output: ImageView,
) -> Result<renderer::graph::PassHandle, renderer::graph::GraphError> {
    graph.raster(
        name,
        &ManagedRasterProgram::textured_mesh(),
        [RasterDraw {
            bindings,
            ..geometry
        }],
        RasterAttachments {
            colors: &[attachment(output)],
            depth: None,
        },
    )
}

#[test]
fn short_lived_per_draw_bindings_connect_two_image_passes_to_sampled_compute() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let mesh = geometry(&mut graph);
    let uniform = uniform(&mut graph);
    let a = color(&mut graph, "a");
    let b = color(&mut graph, "b");
    let output = color(&mut graph, "output");
    seed(&mut graph, "overwritten a", a);
    seed(&mut graph, "current a", a);
    {
        let mut inputs = bindings(uniform, a);
        inputs.swap(0, 2);
        draw(&mut graph, "first", mesh, &inputs, b).unwrap();
        let overwritten = inputs[0];
        inputs.fill(overwritten);
        draw(&mut graph, "second", mesh, &bindings(uniform, b), output).unwrap();
    }
    let result = graph
        .buffer("sampled result", BufferDesc { size: 96 })
        .unwrap();
    {
        let compute = [
            Binding {
                group: 0,
                binding: 0,
                resource: BindingResource::ImageView(output),
            },
            Binding {
                group: 0,
                binding: 1,
                resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
            },
            Binding {
                group: 1,
                binding: 0,
                resource: BindingResource::Buffer {
                    buffer: result,
                    offset: 16,
                    size: 80,
                },
            },
        ];
        graph
            .dispatch(
                "sample compute",
                &ManagedProgram::sampled_float_readback(),
                &compute,
                [1; 3],
            )
            .unwrap();
    }
    graph.readback_buffer("result", result, 16, 64).unwrap();
    let plan = graph.compile().unwrap();
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        [
            "vertices",
            "indices",
            "offset bytes",
            "current a",
            "first",
            "second",
            "sample compute",
            "result"
        ]
    );
    let retained = plan.clone();
    drop(plan);
    assert_eq!(
        pool.begin().unwrap_err().kind(),
        GraphErrorKind::CapacityExhausted
    );
    drop(retained);
    assert!(pool.is_idle());
}

#[test]
fn selected_mip_and_layer_determine_initialization_and_feedback() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for initialized in [false, true] {
        let mut graph = pool.begin().unwrap();
        let mesh = geometry(&mut graph);
        let uniform = uniform(&mut graph);
        let image = graph
            .image(
                "mips",
                ImageDesc {
                    mip_levels: 2,
                    array_layers: 2,
                    ..ImageDesc::rgba8(4, 4)
                },
            )
            .unwrap();
        let source = graph.image_view(image, 1, 0).unwrap();
        let destination = graph.image_view(image, 1, 1).unwrap();
        let other_mip = graph.image_view(image, 0, 0).unwrap();
        seed(&mut graph, "other mip", other_mip);
        if initialized {
            seed(&mut graph, "selected mip", source);
        }
        let pass = draw(
            &mut graph,
            "different layer",
            mesh,
            &bindings(uniform, source),
            destination,
        )
        .unwrap();
        // A sampled compute is an actual consumer of just the selected output.
        let result = graph.buffer("result", BufferDesc { size: 16 }).unwrap();
        graph
            .dispatch(
                "consume selected",
                &ManagedProgram::sampled_readback(),
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::ImageView(destination),
                    },
                    Binding {
                        group: 0,
                        binding: 1,
                        resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: result,
                            offset: 0,
                            size: 16,
                        },
                    },
                ],
                [1; 3],
            )
            .unwrap();
        graph.readback_buffer("observe", result, 0, 16).unwrap();
        if initialized {
            let plan = graph.compile().unwrap();
            assert!(!plan.ordered_pass_names().any(|name| name == "other mip"));
        } else {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(error.pass(), Some(pass));
        }
    }
}

#[test]
fn feedback_in_a_later_draw_is_rejected_and_the_failed_pass_is_rolled_back() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            bindings: 6,
            draws: 2,
            attachments: 2,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let mesh = geometry(&mut graph);
    let uniform = uniform(&mut graph);
    let source = color(&mut graph, "source");
    let output = color(&mut graph, "output");
    seed(&mut graph, "source clear", source);
    let good = bindings(uniform, source);
    let feedback = bindings(uniform, output);
    let error = graph
        .raster(
            "bad later draw",
            &ManagedRasterProgram::textured_mesh(),
            [
                RasterDraw {
                    bindings: &good,
                    ..mesh
                },
                RasterDraw {
                    bindings: &feedback,
                    ..mesh
                },
            ],
            RasterAttachments {
                colors: &[attachment(output)],
                depth: None,
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    graph
        .raster(
            "two valid draws",
            &ManagedRasterProgram::textured_mesh(),
            [
                RasterDraw {
                    bindings: &good,
                    ..mesh
                },
                RasterDraw {
                    bindings: &good,
                    ..mesh
                },
            ],
            RasterAttachments {
                colors: &[attachment(output)],
                depth: None,
            },
        )
        .unwrap();
    graph.readback_image("result", output.image()).unwrap();
    assert!(
        !graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .any(|name| name == "bad later draw")
    );
}

#[test]
fn slot_kind_owner_and_late_binding_capacity_failures_allow_retry() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            bindings: 3,
            draws: 2,
            ..GraphCapacity::default()
        },
        2,
    )
    .unwrap();
    let mut other = pool.begin().unwrap();
    let foreign_view = color(&mut other, "foreign image");
    let foreign_buffer = uniform(&mut other);
    let mut graph = pool.begin().unwrap();
    let mesh = geometry(&mut graph);
    let uniform = uniform(&mut graph);
    let source = color(&mut graph, "source");
    let output = color(&mut graph, "output");
    seed(&mut graph, "seed", source);
    let valid = bindings(uniform, source);
    for variant in 0..3 {
        let mut invalid = valid;
        match variant {
            0 => invalid[2] = invalid[0],
            1 => invalid[1].resource = invalid[2].resource,
            _ => {
                invalid[0].resource = BindingResource::Buffer {
                    buffer: uniform,
                    offset: 32,
                    size: 32,
                };
            }
        }
        assert_eq!(
            draw(&mut graph, "invalid", mesh, &invalid, output)
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
    }
    assert_eq!(
        draw(&mut graph, "missing sampler", mesh, &valid[..2], output)
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    for foreign in [
        Binding {
            group: 1,
            binding: 0,
            resource: BindingResource::ImageView(foreign_view),
        },
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::Buffer {
                buffer: foreign_buffer,
                offset: 0,
                size: 16,
            },
        },
    ] {
        let mut invalid = valid;
        let slot = if foreign.group == 0 { 0 } else { 1 };
        invalid[slot] = foreign;
        assert_eq!(
            draw(&mut graph, "foreign", mesh, &invalid, output)
                .unwrap_err()
                .kind(),
            GraphErrorKind::WrongOwner
        );
    }
    drop(other);
    let error = graph
        .raster(
            "too many",
            &ManagedRasterProgram::textured_mesh(),
            [
                RasterDraw {
                    bindings: &valid,
                    ..mesh
                },
                RasterDraw {
                    bindings: &valid,
                    ..mesh
                },
            ],
            RasterAttachments {
                colors: &[attachment(output)],
                depth: None,
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
    assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Bindings);
    draw(&mut graph, "retry", mesh, &valid, output).unwrap();
    graph.readback_image("observe", output.image()).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .filter(|name| *name == "retry")
            .count(),
        1
    );
}

#[test]
fn distinct_materials_in_one_pass_keep_both_texture_producers() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let mesh = geometry(&mut graph);
    let uniform = uniform(&mut graph);
    let a = color(&mut graph, "material a");
    let b = color(&mut graph, "material b");
    let output = color(&mut graph, "output");
    seed(&mut graph, "texture a", a);
    seed(&mut graph, "texture b", b);
    {
        let a_bindings = bindings(uniform, a);
        let b_bindings = bindings(uniform, b);
        graph
            .raster(
                "different materials",
                &ManagedRasterProgram::textured_mesh(),
                [
                    RasterDraw {
                        bindings: &a_bindings,
                        ..mesh
                    },
                    RasterDraw {
                        bindings: &b_bindings,
                        ..mesh
                    },
                ],
                RasterAttachments {
                    colors: &[attachment(output)],
                    depth: None,
                },
            )
            .unwrap();
    }
    graph.readback_image("result", output.image()).unwrap();
    let plan = graph.compile().unwrap();
    let names = plan.ordered_pass_names().collect::<Vec<_>>();
    assert!(names.contains(&"texture a"));
    assert!(names.contains(&"texture b"));
}

#[test]
fn sampled_reader_and_later_attachment_writer_cannot_be_reordered() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for contradictory_order in [false, true] {
        let mut graph = pool.begin().unwrap();
        let mesh = geometry(&mut graph);
        let uniform = uniform(&mut graph);
        let source = color(&mut graph, "source");
        let output = color(&mut graph, "output");
        seed(&mut graph, "initial texture", source);
        let reader = draw(
            &mut graph,
            "sample before overwrite",
            mesh,
            &bindings(uniform, source),
            output,
        )
        .unwrap();
        let writer = graph
            .attachment_pass(
                "overwrite texture",
                RasterAttachments {
                    colors: &[attachment(source)],
                    depth: None,
                },
            )
            .unwrap();
        graph
            .readback_image("old texture result", output.image())
            .unwrap();
        graph
            .readback_image("new texture result", source.image())
            .unwrap();
        if contradictory_order {
            graph.depends_on(reader, writer).unwrap();
            assert_eq!(
                graph.compile().unwrap_err().kind(),
                GraphErrorKind::DependencyCycle
            );
        } else {
            let plan = graph.compile().unwrap();
            let names = plan.ordered_pass_names().collect::<Vec<_>>();
            let reader = names
                .iter()
                .position(|&name| name == "sample before overwrite")
                .unwrap();
            let writer = names
                .iter()
                .position(|&name| name == "overwrite texture")
                .unwrap();
            assert!(reader < writer);
        }
    }
}

#[test]
fn sampled_compute_does_not_initialize_descriptor_tail() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for observe_tail in [false, true] {
        let mut graph = pool.begin().unwrap();
        let source = color(&mut graph, "source");
        seed(&mut graph, "seed", source);
        let result = graph.buffer("output", BufferDesc { size: 96 }).unwrap();
        graph
            .dispatch(
                "sample",
                &ManagedProgram::sampled_float_readback(),
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::ImageView(source),
                    },
                    Binding {
                        group: 0,
                        binding: 1,
                        resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: result,
                            offset: 0,
                            size: 96,
                        },
                    },
                ],
                [1; 3],
            )
            .unwrap();
        graph
            .readback_buffer("observe", result, if observe_tail { 64 } else { 0 }, 16)
            .unwrap();
        if observe_tail {
            assert_eq!(
                graph.compile().unwrap_err().kind(),
                GraphErrorKind::UninitializedRead
            );
        } else {
            assert_eq!(graph.compile().unwrap().pass_count(), 3);
        }
    }
}

#[test]
fn sampled_depth_requires_stored_depth_contents() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for store in [StoreOp::Store, StoreOp::Discard] {
        let mut graph = pool.begin().unwrap();
        let mesh = geometry(&mut graph);
        let uniform = uniform(&mut graph);
        let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        let view = graph.image_view(depth, 0, 0).unwrap();
        graph
            .attachment_pass(
                "depth seed",
                RasterAttachments {
                    colors: &[],
                    depth: Some(Attachment {
                        view,
                        load: LoadOp::Clear(0.25),
                        store,
                    }),
                },
            )
            .unwrap();
        let output = color(&mut graph, "output");
        let pass = draw(
            &mut graph,
            "sample depth",
            mesh,
            &bindings(uniform, view),
            output,
        )
        .unwrap();
        graph.readback_image("result", output.image()).unwrap();
        if store == StoreOp::Discard {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(error.pass(), Some(pass));
            assert_eq!(
                error.range(),
                Some(&renderer::graph::AccessRange::Image {
                    mip: 0,
                    layer: 0,
                    aspect: renderer::graph::ImageAspect::Depth,
                })
            );
        } else {
            assert!(
                graph
                    .compile()
                    .unwrap()
                    .ordered_pass_names()
                    .any(|name| name == "depth seed")
            );
        }
    }
}

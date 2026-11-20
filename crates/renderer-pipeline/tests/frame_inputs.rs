use renderer::capacity::ResourceClass;
use renderer::graph::{
    Attachment, BufferDesc, GraphCapacity, GraphErrorKind, GraphPool, ImageDesc, LoadOp,
    RasterAttachments, RasterDraw, RasterParameters, SampleCount, StoreOp,
};
use renderer::program::{ManagedProgram, ManagedRasterProgram};
use renderer_pipeline::{
    DrawCandidate, FrameInputs, FrameMaterial, FrameResource, FrameSampledView, FrameView,
    PipelineError, PipelineRequirements, PrepareOptions, PreparedPipeline, Program, ResourceDesc,
    ShaderRequest,
};

const PIPELINE: &str = "
version: 1
resources:
  - {id: output, kind: image, source: external, extent: frame, format: rgba8_unorm}
draw_lists:
  - {id: main.opaque, view: main, queue: opaque}
passes:
  - id: opaque
    kind: raster
    shader: mesh
    draw_list: main.opaque
    colors: [{image: output, load: clear, clear: [0, 0, 0, 1]}]
output: output
";

#[test]
fn wrong_input_and_duplicate_view_return_the_graph_slot() {
    let mut pipeline = PreparedPipeline::from_yaml(
        PIPELINE,
        PrepareOptions {
            frame_extent: Some([8, 4]),
            frames_in_flight: 1,
            max_draws_per_list: 1,
            max_materials: 0,
        },
        |_| Ok(Program::Raster(ManagedRasterProgram::mesh())),
    )
    .unwrap();
    assert_eq!(pipeline.requirements().draw_lists, 1);
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let view = FrameView {
        name: "main",
        candidates: &[],
    };

    let mut graph = pool.begin().unwrap();
    let wrong = graph.image("frame image", ImageDesc::rgba8(4, 8)).unwrap();
    let failure = pipeline.build(
        graph,
        FrameInputs {
            resources: &[FrameResource {
                name: "output",
                resource: wrong.into(),
            }],
            views: &[view],
            ..FrameInputs::default()
        },
    );
    assert!(
        failure
            .err()
            .unwrap()
            .to_string()
            .contains("prepared description")
    );

    let mut graph = pool.begin().unwrap();
    let image = graph.image("frame image", ImageDesc::rgba8(8, 4)).unwrap();
    let failure = pipeline.build(
        graph,
        FrameInputs {
            resources: &[FrameResource {
                name: "output",
                resource: image.into(),
            }],
            views: &[view, view],
            ..FrameInputs::default()
        },
    );
    assert!(
        failure
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate view 'main'")
    );

    let mut graph = pool.begin().unwrap();
    let image = graph.image("frame image", ImageDesc::rgba8(8, 4)).unwrap();
    let (mut graph, output) = pipeline
        .build(
            graph,
            FrameInputs {
                resources: &[FrameResource {
                    name: "output",
                    resource: image.into(),
                }],
                views: &[view],
                ..FrameInputs::default()
            },
        )
        .unwrap();
    assert_eq!(output.image(), image);
    assert_eq!(output.named_image("output"), Some(image));
    assert_eq!(output.named_image("undeclared"), None);
    graph.export_image(output.image()).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["opaque"]
    );
}

#[test]
fn static_requirements_and_descriptors_resolve_for_each_extent() {
    let yaml = "
version: 1
resources:
  - {id: hdr, kind: image, source: external, extent: frame, format: rgba16_float}
  - {id: mapped, kind: image, extent: frame, format: rgba16_float}
passes:
  - id: neutral
    kind: raster
    shader: tone
    constants: [1073741824]
    colors: [{image: mapped, load: clear, clear: [0, 0, 0, 0]}]
    bindings:
      - {group: 0, binding: 0, kind: image, resource: hdr}
      - {group: 0, binding: 1, kind: sampler, sampler: nearest_clamp}
output: mapped
";
    let external_output = yaml.replace(
        "id: mapped, kind: image,",
        "id: mapped, kind: image, source: external,",
    );
    for [width, height] in [[8, 4], [17, 11]] {
        let tone = renderer_pipeline::standard::passes::tone_map_neutral_pipeline();
        let prepare = |yaml: &str| {
            PreparedPipeline::from_yaml(
                yaml,
                PrepareOptions {
                    frame_extent: Some([width, height]),
                    frames_in_flight: 2,
                    max_draws_per_list: 0,
                    max_materials: 0,
                },
                |_| Ok(Program::Raster(tone.program.clone())),
            )
            .unwrap()
        };
        let pipeline = prepare(yaml);
        let desc = ImageDesc::rgba16_float(width, height);
        assert!(
            pipeline
                .external_resources()
                .eq([("hdr", ResourceDesc::Image(desc))])
        );
        assert_eq!(pipeline.output_resource(), ("mapped", desc));
        let working_set = pipeline.working_set();
        assert!(matches!(
            working_set.resources,
            [ResourceClass::Image { desc: prepared, count: 2 }] if *prepared == desc
        ));
        assert_eq!(working_set.raster.len(), 1);
        assert!(working_set.compute.is_empty());
        assert_eq!(
            pipeline.requirements(),
            PipelineRequirements {
                resources: 1,
                passes: 1,
                compute_dispatches: 0,
                draw_lists: 0,
                draws: 1,
                bindings: 2,
                accesses: 4,
                attachments: 1,
                raster_constant_words: 1,
                name_bytes: "mapped".len() + "neutral".len(),
                writes_external_resources: false,
            },
        );
        assert!(
            prepare(&external_output)
                .requirements()
                .writes_external_resources
        );
    }
}

#[test]
fn static_mixed_pipeline_accounts_for_compute_writes_and_scaled_images() {
    let yaml = "
version: 1
resources:
  - {id: hdr, kind: image, source: external, extent: frame, format: rgba16_float}
  - {id: probe, kind: buffer, source: external, bytes: 64}
  - {id: mapped, kind: image, extent: frame, divisor: 4, format: rgba16_float}
passes:
  - id: sample
    kind: compute
    shader: sample
    bindings:
      - {group: 0, binding: 0, kind: image, resource: hdr}
      - {group: 0, binding: 1, kind: sampler, sampler: nearest_clamp}
      - {group: 1, binding: 0, kind: buffer, resource: probe, offset: 0, size: 64}
  - id: neutral
    kind: raster
    shader: tone
    constants: [1073741824]
    colors: [{image: mapped, load: clear, clear: [0, 0, 0, 0]}]
    bindings:
      - {group: 0, binding: 0, kind: image, resource: hdr}
      - {group: 0, binding: 1, kind: sampler, sampler: nearest_clamp}
output: mapped
";
    for (frame_extent, scaled) in [([16, 12], [4, 3]), ([17, 11], [5, 3]), ([1, 2], [1, 1])] {
        let pipeline = PreparedPipeline::from_yaml(
            yaml,
            PrepareOptions {
                frame_extent: Some(frame_extent),
                frames_in_flight: 1,
                max_draws_per_list: 0,
                max_materials: 0,
            },
            |request| match request {
                ShaderRequest::Program("sample") => {
                    Ok(Program::Compute(ManagedProgram::sampled_float_readback()))
                }
                ShaderRequest::Program("tone") => Ok(Program::Raster(
                    renderer_pipeline::standard::passes::tone_map_neutral_pipeline().program,
                )),
                _ => unreachable!(),
            },
        )
        .unwrap();
        let output = ImageDesc::rgba16_float(scaled[0], scaled[1]);
        assert_eq!(pipeline.image_resource("mapped"), Some(output));
        assert_eq!(pipeline.image_resource("probe"), None);
        assert_eq!(pipeline.output_resource(), ("mapped", output));
        assert_eq!(
            pipeline.requirements(),
            PipelineRequirements {
                resources: 1,
                passes: 2,
                compute_dispatches: 1,
                draw_lists: 0,
                draws: 1,
                bindings: 5,
                accesses: 7,
                attachments: 1,
                raster_constant_words: 1,
                name_bytes: "mappedsampleneutral".len(),
                writes_external_resources: true,
            },
        );
    }
}

#[test]
fn draw_list_overflow_returns_capacity_error_before_capturing_draws() {
    let mut pipeline = PreparedPipeline::from_yaml(
        PIPELINE,
        PrepareOptions {
            frame_extent: Some([8, 4]),
            frames_in_flight: 1,
            max_draws_per_list: 1,
            max_materials: 0,
        },
        |_| Ok(Program::Raster(ManagedRasterProgram::mesh())),
    )
    .unwrap();
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let buffer = graph.buffer("geometry", BufferDesc { size: 16 }).unwrap();
    let image = graph.image("output", ImageDesc::rgba8(8, 4)).unwrap();
    let candidate = DrawCandidate {
        queue: "opaque",
        material: None,
        draw: RasterDraw {
            bindings: None,
            vertices: buffer,
            indices: buffer,
            first_index: 0,
            index_count: 3,
            instance_count: 1,
            parameters: RasterParameters::default().into(),
        },
    };

    let failure = pipeline.build(
        graph,
        FrameInputs {
            resources: &[FrameResource {
                name: "output",
                resource: image.into(),
            }],
            views: &[FrameView {
                name: "main",
                candidates: &[candidate, candidate],
            }],
            ..FrameInputs::default()
        },
    );
    assert!(
        failure
            .err()
            .unwrap()
            .to_string()
            .contains("exceeds prepared capacity 1")
    );
    assert!(pool.begin().is_ok());
}

#[test]
fn missing_material_binding_identifies_the_pass_and_releases_the_slot() {
    let definition = PIPELINE.replace(
        "shader: mesh",
        "shader_template: mesh
    material_types: [paint]
    bindings:
      - {kind: material, group: 0, binding: 0, resource: offset}
      - {kind: material, group: 1, binding: 0, resource: base_color}
      - {kind: sampler, group: 1, binding: 1, sampler: nearest_clamp}",
    );
    let mut pipeline = PreparedPipeline::from_yaml(
        &definition,
        PrepareOptions {
            frame_extent: Some([8, 4]),
            frames_in_flight: 1,
            max_draws_per_list: 4,
            max_materials: 2,
        },
        |_| Ok(Program::Raster(ManagedRasterProgram::textured_mesh())),
    )
    .unwrap();
    let requirements = pipeline.requirements();
    assert_eq!(requirements.draws, 4);
    assert_eq!(requirements.bindings, 6);
    assert_eq!(requirements.accesses, 16);
    assert_eq!(requirements.raster_constant_words, 80);

    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for (bindings, reason) in [
        (0..0, "missing material binding 'offset'"),
        (0..1, "binding range is outside the frame material bindings"),
    ] {
        let mut graph = pool.begin().unwrap();
        let geometry = graph.buffer("geometry", BufferDesc { size: 16 }).unwrap();
        let image = graph.image("output", ImageDesc::rgba8(8, 4)).unwrap();
        let candidate = DrawCandidate {
            queue: "opaque",
            material: Some(0),
            draw: RasterDraw {
                bindings: None,
                vertices: geometry,
                indices: geometry,
                first_index: 0,
                index_count: 3,
                instance_count: 1,
                parameters: RasterParameters::default().into(),
            },
        };
        let result = pipeline.build(
            graph,
            FrameInputs {
                resources: &[FrameResource {
                    name: "output",
                    resource: image.into(),
                }],
                views: &[FrameView {
                    name: "main",
                    candidates: &[candidate],
                }],
                materials: &[FrameMaterial {
                    material_type: "paint",
                    bindings,
                }],
                ..FrameInputs::default()
            },
        );
        let message = result.err().unwrap().to_string();
        assert!(
            message.contains("pass 'opaque', material 0 ('paint')"),
            "{message}"
        );
        assert!(message.contains(reason), "{message}");
    }
    assert!(pool.begin().is_ok());
}

const SAMPLED_VIEW_PIPELINE: &str = "
version: 1
resources:
  - {id: mapped, kind: image, width: 4, height: 4, format: rgba16_float}
passes:
  - id: neutral
    kind: raster
    shader: tone
    constants: [1073741824]
    colors: [{image: mapped, load: clear, clear: [0, 0, 0, 0]}]
    bindings:
      - {group: 0, binding: 0, kind: sampled_view, resource: hdr}
      - {group: 0, binding: 1, kind: sampler, sampler: nearest_clamp}
output: mapped
";

fn prepare_tone_pipeline(yaml: &str) -> Result<PreparedPipeline, PipelineError> {
    PreparedPipeline::from_yaml(
        yaml,
        PrepareOptions {
            frame_extent: None,
            frames_in_flight: 1,
            max_draws_per_list: 0,
            max_materials: 0,
        },
        |_| {
            Ok(Program::Raster(
                renderer_pipeline::standard::passes::tone_map_neutral_pipeline().program,
            ))
        },
    )
}

#[test]
fn shared_sampled_view_retains_every_mip_in_the_graph_read() {
    let mut pipeline = prepare_tone_pipeline(SAMPLED_VIEW_PIPELINE).unwrap();
    assert!(pipeline.sampled_view_names().eq(["hdr"]));
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for initialize_all in [false, true] {
        let mut graph = pool.begin().unwrap();
        let image = graph
            .image(
                "scene reflection",
                ImageDesc {
                    mip_levels: 2,
                    ..ImageDesc::rgba16_float(4, 4)
                },
            )
            .unwrap();
        if initialize_all {
            graph.upload_image("all mips", image, &[0; 160]).unwrap();
        } else {
            let base = graph.image_view(image, 0, 0).unwrap();
            graph
                .attachment_pass(
                    "base mip",
                    RasterAttachments {
                        colors: &[Attachment {
                            view: base,
                            load: LoadOp::Clear([0.0; 4]),
                            store: StoreOp::Store,
                        }],
                        color_resolves: &[],
                        depth: None,
                    },
                )
                .unwrap();
        }
        let view = graph.sampled_image_view(image, 0, 2).unwrap();
        let (mut graph, output) = pipeline
            .build(
                graph,
                FrameInputs {
                    sampled_views: &[FrameSampledView { name: "hdr", view }],
                    ..FrameInputs::default()
                },
            )
            .unwrap();
        graph.export_image(output.image()).unwrap();
        let plan = graph.compile();
        if initialize_all {
            assert_eq!(
                plan.unwrap().ordered_pass_names().collect::<Vec<_>>(),
                ["all mips", "neutral"]
            );
        } else {
            assert_eq!(
                plan.err().unwrap().kind(),
                GraphErrorKind::UninitializedRead
            );
        }
    }
}

#[test]
fn shared_sampled_view_requires_unique_local_input_and_sampled_abi() {
    let wrong_abi = SAMPLED_VIEW_PIPELINE.replace(
        "kind: sampler, sampler: nearest_clamp",
        "kind: sampled_view, resource: hdr",
    );
    let error = prepare_tone_pipeline(&wrong_abi).err().unwrap();
    assert!(error.to_string().contains("resource kind differs"));

    let mut pipeline = prepare_tone_pipeline(SAMPLED_VIEW_PIPELINE).unwrap();
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let error = pipeline
        .build(pool.begin().unwrap(), FrameInputs::default())
        .err()
        .unwrap();
    assert!(error.to_string().contains("missing sampled view 'hdr'"));
    let mut graph = pool.begin().unwrap();
    let image = graph
        .image("source", ImageDesc::rgba16_float(4, 4))
        .unwrap();
    let view = FrameSampledView {
        name: "hdr",
        view: graph.image_view(image, 0, 0).unwrap(),
    };
    let error = pipeline
        .build(
            graph,
            FrameInputs {
                sampled_views: &[view, view],
                ..FrameInputs::default()
            },
        )
        .err()
        .unwrap();
    assert!(error.to_string().contains("duplicate sampled view 'hdr'"));

    let error = pipeline
        .build(
            pool.begin().unwrap(),
            FrameInputs {
                sampled_views: &[view],
                ..FrameInputs::default()
            },
        )
        .err()
        .unwrap();
    assert!(
        matches!(error, PipelineError::Graph(error) if error.kind() == GraphErrorKind::WrongOwner)
    );
    assert!(pool.begin().is_ok());
}

#[test]
fn indexed_capacity_overflow_precedes_frame_scratch_reservation() {
    let error = PreparedPipeline::from_yaml(
        PIPELINE,
        PrepareOptions {
            frame_extent: Some([8, 4]),
            frames_in_flight: 1,
            max_draws_per_list: usize::MAX,
            max_materials: 0,
        },
        |_| Ok(Program::Raster(ManagedRasterProgram::mesh())),
    )
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("Graph declaration limits overflow")
    );
}

const MSAA_PIPELINE: &str = "
version: 1
resources:
  - {id: multisampled, kind: image, width: 8, height: 4, format: rgba8_unorm, samples: 4}
  - {id: resolved, kind: image, width: 8, height: 4, format: rgba8_unorm}
draw_lists:
  - {id: first, view: main, queue: opaque}
  - {id: second, view: overlay, queue: opaque}
raster_batches:
  scene:
    - {shader: mesh, draw_list: first}
    - {shader: mesh, draw_list: second}
passes:
  - id: clear
    kind: attachment
    colors: [{image: multisampled, load: clear, clear: [0, 0, 0, 1]}]
  - id: resolve_empty
    kind: attachment
    when: {draw_list: first, empty: true}
    colors: [{image: multisampled, load: load, store: discard, resolve: resolved}]
  - id: draw_resolve
    kind: raster
    batches: scene
    when: {draw_list: first, empty: false}
    colors: [{image: multisampled, load: load, store: discard, resolve: resolved}]
output: resolved
";

const MSAA_OPTIONS: PrepareOptions = PrepareOptions {
    frame_extent: None,
    frames_in_flight: 1,
    max_draws_per_list: 1,
    max_materials: 0,
};

#[test]
fn msaa_attachment_branch_and_batched_draw_branch_produce_the_same_named_output() {
    let (resources, batches_and_passes) = MSAA_PIPELINE.split_once("raster_batches:").unwrap();
    let (batches, passes) = batches_and_passes.split_once("passes:").unwrap();
    let batches = format!("version: 1\nraster_batches:{batches}");
    let passes = format!("version: 1\npasses:{passes}");
    let prepare = |fragments: &[&str]| {
        PreparedPipeline::from_yaml_fragments(fragments, MSAA_OPTIONS, Default::default(), |_| {
            Ok(Program::Raster(ManagedRasterProgram::mesh()))
        })
    };
    let mut pipeline = prepare(&[resources, &batches, &passes]).unwrap();
    let duplicate = prepare(&[resources, &batches, &batches, &passes])
        .err()
        .unwrap();
    assert!(
        duplicate
            .to_string()
            .contains("duplicate raster batch list")
    );
    assert_eq!(
        pipeline.working_set().raster[0].sample_count,
        SampleCount::X4
    );
    assert_eq!(pipeline.requirements().draws, 2);
    assert_eq!(pipeline.requirements().attachments, 3);
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for has_draws in [false, true, false] {
        let mut graph = pool.begin().unwrap();
        let mut candidates = Vec::new();
        if has_draws {
            for (vertex_upload, index_upload) in
                [("vertices A", "indices A"), ("vertices B", "indices B")]
            {
                let vertices = graph
                    .buffer(vertex_upload, BufferDesc { size: 84 })
                    .unwrap();
                let indices = graph.buffer(index_upload, BufferDesc { size: 12 }).unwrap();
                graph
                    .upload_buffer(vertex_upload, vertices, 0, &[0; 84])
                    .unwrap();
                graph
                    .upload_buffer(index_upload, indices, 0, &[0; 12])
                    .unwrap();
                candidates.push(DrawCandidate {
                    queue: "opaque",
                    material: None,
                    draw: RasterDraw {
                        bindings: None,
                        vertices,
                        indices,
                        first_index: 0,
                        index_count: 3,
                        instance_count: 1,
                        parameters: RasterParameters::default().into(),
                    },
                });
            }
        }
        let mut views = vec![FrameView {
            name: "main",
            candidates: &candidates[..usize::from(has_draws)],
        }];
        if has_draws {
            views.push(FrameView {
                name: "overlay",
                candidates: &candidates[1..],
            });
        }
        let (mut graph, output) = pipeline
            .build(
                graph,
                FrameInputs {
                    views: &views,
                    ..FrameInputs::default()
                },
            )
            .unwrap();
        assert_eq!(output.named_image("resolved"), Some(output.image()));
        assert_eq!(
            graph.image_desc(output.image()).unwrap(),
            ImageDesc::rgba8(8, 4)
        );
        graph.export_image(output.image()).unwrap();
        let plan = graph.compile().unwrap();
        let names = plan.ordered_pass_names().collect::<Vec<_>>();
        if has_draws {
            assert_eq!(
                names,
                [
                    "vertices A",
                    "indices A",
                    "vertices B",
                    "indices B",
                    "clear",
                    "draw_resolve"
                ]
            );
        } else {
            assert_eq!(names, ["clear", "resolve_empty"]);
        }
    }
}

#[test]
fn raster_shader_is_resolved_once_when_used_at_both_sample_counts() {
    let yaml = MSAA_PIPELINE.replace(
        "output: resolved",
        "  - {id: draw_1x, kind: raster, shader: mesh, draw_list: first, colors: [{image: resolved, load: load}]}\noutput: resolved",
    );
    let mut resolves = 0;
    let pipeline = PreparedPipeline::from_yaml(&yaml, MSAA_OPTIONS, |_| {
        resolves += 1;
        Ok(Program::Raster(ManagedRasterProgram::mesh()))
    })
    .unwrap();
    assert_eq!(resolves, 1);
    assert!(
        pipeline
            .working_set()
            .raster
            .iter()
            .map(|variant| variant.sample_count)
            .eq([SampleCount::X4, SampleCount::X1,])
    );
}

#[test]
fn incompatible_resolves_are_rejected_at_preparation() {
    for (from, to, reason) in [
        ("samples: 4", "samples: 2", "samples must be 1 or 4"),
        ("samples: 4", "samples: 1", "matching 1x destination"),
        (
            "store: discard, resolve: resolved",
            "store: discard, resolve: multisampled",
            "duplicate attachment",
        ),
        (
            "load: load, store: discard",
            "load: discard, store: discard",
            "initialized 4x color",
        ),
    ] {
        let yaml = MSAA_PIPELINE.replace(from, to);
        let error = PreparedPipeline::from_yaml(&yaml, MSAA_OPTIONS, |_| {
            Ok(Program::Raster(ManagedRasterProgram::mesh()))
        })
        .err()
        .unwrap();
        assert!(error.to_string().contains(reason), "{error}");
    }
}

use renderer::capacity::ResourceClass;
use renderer::graph::{
    BufferDesc, GraphCapacity, GraphPool, ImageDesc, RasterDraw, RasterParameters,
};
use renderer::program::{ManagedProgram, ManagedRasterProgram};
use renderer_pipeline::{
    DrawCandidate, FrameInputs, FrameMaterial, FrameResource, FrameView, PrepareOptions,
    PreparedPipeline, Program, ResourceDesc, ShaderRequest, StaticRequirements,
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
    assert!(pipeline.static_requirements().is_none());
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
        let tone = renderer::standard::passes::tone_map_neutral_pipeline();
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
            pipeline.static_requirements(),
            Some(StaticRequirements {
                resources: 1,
                passes: 1,
                draws: 1,
                bindings: 2,
                attachments: 1,
                raster_constant_words: 1,
                name_bytes: "mapped".len() + "neutral".len(),
                writes_external_resources: false,
            }),
        );
        assert!(
            prepare(&external_output)
                .static_requirements()
                .unwrap()
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
                    renderer::standard::passes::tone_map_neutral_pipeline().program,
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
            pipeline.static_requirements(),
            Some(StaticRequirements {
                resources: 1,
                passes: 2,
                draws: 1,
                bindings: 5,
                attachments: 1,
                raster_constant_words: 1,
                name_bytes: "mappedsampleneutral".len(),
                writes_external_resources: true,
            }),
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
            max_draws_per_list: 1,
            max_materials: 1,
        },
        |_| Ok(Program::Raster(ManagedRasterProgram::textured_mesh())),
    )
    .unwrap();
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
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
                bindings: &[],
            }],
        },
    );
    let message = result.err().unwrap().to_string();
    assert!(
        message.contains("pass 'opaque', material 0 ('paint')"),
        "{message}"
    );
    assert!(
        message.contains("missing material binding 'offset'"),
        "{message}"
    );
    assert!(pool.begin().is_ok());
}

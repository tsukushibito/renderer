use renderer::capacity::ResourceClass;
use renderer::graph::{
    BufferDesc, GraphCapacity, GraphPool, ImageDesc, RasterDraw, RasterParameters,
};
use renderer::program::ManagedRasterProgram;
use renderer_pipeline::{
    DrawCandidate, FrameInputs, FrameMaterial, FrameResource, FrameView, FullscreenRequirements,
    PrepareOptions, PreparedPipeline, Program, ResourceDesc,
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
    assert!(pipeline.fullscreen_requirements().is_none());
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
    assert_eq!(output, image);
    graph.export_image(output).unwrap();
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
fn fullscreen_requirements_and_descriptors_resolve_for_each_extent() {
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
            pipeline.fullscreen_requirements(),
            Some(FullscreenRequirements {
                resources: 1,
                passes: 1,
                bindings: 2,
                attachments: 1,
                raster_constant_words: 1,
                name_bytes: "mapped".len() + "neutral".len(),
                writes_external_attachments: false,
            }),
        );
        assert!(
            prepare(&external_output)
                .fullscreen_requirements()
                .unwrap()
                .writes_external_attachments
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

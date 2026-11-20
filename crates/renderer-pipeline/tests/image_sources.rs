#![cfg(feature = "yaml")]

use renderer::capacity::ResourceClass;
use renderer::graph::{
    Attachment, GraphCapacity, GraphErrorKind, GraphPool, ImageDesc, LoadOp, RasterAttachments,
    StoreOp,
};
use renderer::program::ManagedRasterProgram;
use renderer_pipeline::{
    FrameImageSource, FrameInputs, FrameView, ImageSource, PipelineError, PrepareOptions,
    PreparedPipeline, Program, ShaderRequest,
};

const PIPELINE: &str = "
version: 1
resources:
  - id: cascade_a
    kind: image
    format: depth32_float
    width: 8
    height: 8
    source: {render: shadow_a, reuse: frame_input}
  - id: cascade_b
    kind: image
    format: depth32_float
    width: 8
    height: 8
    source: {render: shadow_b, reuse: frame_input}
draw_lists:
  - {id: casters_a, view: a, queue: shadow}
  - {id: casters_b, view: b, queue: shadow}
passes:
  - id: shadow_a
    kind: raster
    shader: shadow
    draw_list: casters_a
    depth: {image: cascade_a, load: clear, clear: 1, store: store}
  - id: shadow_b
    kind: raster
    shader: shadow
    draw_list: casters_b
    depth: {image: cascade_b, load: clear, clear: 1, store: store}
output: cascade_a
";

const COLOR_PIPELINE: &str = "
version: 1
resources:
  - {id: cached, kind: image, format: rgba8_unorm, width: 8, height: 8, source: {render: draw, reuse: frame_input}}
  - {id: msaa, kind: image, format: rgba8_unorm, width: 8, height: 8, samples: 4}
  - {id: mapped, kind: image, format: rgba16_float, width: 8, height: 8}
draw_lists:
  - {id: meshes, view: a, queue: opaque}
passes:
  - id: draw
    kind: raster
    shader: mesh
    draw_list: meshes
    colors: [{image: cached, load: clear, clear: [0, 0, 0, 1], store: store}]
output: cached
";

const RENDER_FRESH: ImageSource = ImageSource::Render { target: None };
const FRESH_A: FrameImageSource<'_> = FrameImageSource {
    name: "cascade_a",
    source: RENDER_FRESH,
};
const FRESH_B: FrameImageSource<'_> = FrameImageSource {
    name: "cascade_b",
    source: RENDER_FRESH,
};
const VIEW_A: FrameView<'_> = FrameView {
    name: "a",
    candidates: &[],
};
const VIEW_B: FrameView<'_> = FrameView {
    name: "b",
    candidates: &[],
};

fn prepare(yaml: &str) -> Result<PreparedPipeline, PipelineError> {
    PreparedPipeline::from_yaml(
        yaml,
        PrepareOptions {
            frame_extent: None,
            frames_in_flight: 2,
            max_draws_per_list: 1,
            max_materials: 0,
        },
        |request| {
            let program = match request {
                ShaderRequest::Program("shadow") => ManagedRasterProgram::depth_only(),
                ShaderRequest::Program("mesh") => ManagedRasterProgram::mesh(),
                ShaderRequest::Program("tone") => {
                    renderer_pipeline::standard::passes::tone_map_neutral_pipeline().program
                }
                _ => unreachable!("test fixture uses known shaders"),
            };
            Ok(Program::Raster(program))
        },
    )
}

#[test]
fn reuse_omits_only_its_producer_and_view_and_retains_the_all_render_capacity() {
    let mut pipeline = prepare(PIPELINE).unwrap();
    let desc = ImageDesc::depth32(8, 8);
    assert!(
        pipeline
            .selectable_images()
            .eq([("cascade_a", desc), ("cascade_b", desc)])
    );
    assert_eq!(pipeline.requirements().passes, 2);
    assert_eq!(pipeline.requirements().resources, 2);
    assert!(pipeline.requirements().writes_external_resources);
    assert!(matches!(
        pipeline.working_set().resources,
        [ResourceClass::Image { desc: prepared, count: 4 }] if *prepared == desc
    ));

    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let existing = graph.image("cached depth", desc).unwrap();
    let view = graph.image_view(existing, 0, 0).unwrap();
    graph
        .attachment_pass(
            "prior depth",
            RasterAttachments {
                colors: &[],
                color_resolves: &[],
                depth: Some(Attachment {
                    view,
                    load: LoadOp::Clear(0.5),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    let target = graph.image("existing redraw target", desc).unwrap();
    let (mut graph, output) = pipeline
        .build(
            graph,
            FrameInputs {
                image_sources: &[
                    FrameImageSource {
                        source: ImageSource::Reuse(existing),
                        ..FRESH_A
                    },
                    FrameImageSource {
                        source: ImageSource::Render {
                            target: Some(target),
                        },
                        ..FRESH_B
                    },
                ],
                views: &[VIEW_B],
                ..FrameInputs::default()
            },
        )
        .unwrap();
    assert_eq!(output.image(), existing);
    assert_eq!(output.named_image("cascade_b"), Some(target));
    graph.export_image(output.image()).unwrap();
    graph.export_image(target).unwrap();
    assert!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .eq(["prior depth", "shadow_b"])
    );

    let (mut graph, output) = pipeline
        .build(
            pool.begin().unwrap(),
            FrameInputs {
                image_sources: &[FRESH_A, FRESH_B],
                views: &[VIEW_A, VIEW_B],
                ..FrameInputs::default()
            },
        )
        .unwrap();
    graph.export_image(output.image()).unwrap();
    graph
        .export_image(output.named_image("cascade_b").unwrap())
        .unwrap();
    assert!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .eq(["shadow_a", "shadow_b"])
    );
}

#[test]
fn a_selectable_image_requires_exactly_one_choice() {
    let mut pipeline = prepare(PIPELINE).unwrap();
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let unknown = FrameImageSource {
        name: "typo",
        ..FRESH_B
    };
    for (inputs, expected) in [
        (&[FRESH_A][..], "missing image source for \"cascade_b\""),
        (
            &[FRESH_A, FRESH_A],
            "duplicate image source for \"cascade_a\"",
        ),
        (&[FRESH_A, unknown], "unknown selectable image \"typo\""),
    ] {
        let error = pipeline
            .build(
                pool.begin().unwrap(),
                FrameInputs {
                    image_sources: inputs,
                    ..FrameInputs::default()
                },
            )
            .err()
            .unwrap();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn supplied_images_must_have_the_prepared_descriptor_and_belong_to_the_graph() {
    let mut pipeline = prepare(PIPELINE).unwrap();
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let wrong = graph
        .image("wrong extent", ImageDesc::depth32(8, 4))
        .unwrap();
    let foreign = graph
        .image("other graph", ImageDesc::depth32(8, 8))
        .unwrap();
    let error = pipeline
        .build(
            graph,
            FrameInputs {
                image_sources: &[
                    FrameImageSource {
                        source: ImageSource::Render {
                            target: Some(wrong),
                        },
                        ..FRESH_A
                    },
                    FRESH_B,
                ],
                ..FrameInputs::default()
            },
        )
        .err()
        .unwrap();
    assert!(error.to_string().contains("prepared description"));

    let error = pipeline
        .build(
            pool.begin().unwrap(),
            FrameInputs {
                image_sources: &[
                    FrameImageSource {
                        source: ImageSource::Reuse(foreign),
                        ..FRESH_A
                    },
                    FRESH_B,
                ],
                ..FrameInputs::default()
            },
        )
        .err()
        .unwrap();
    assert!(
        matches!(error, PipelineError::Graph(error) if error.kind() == GraphErrorKind::WrongOwner)
    );
}

#[test]
fn reuse_does_not_initialize_an_image() {
    let mut pipeline = prepare(PIPELINE).unwrap();
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let uninitialized = graph
        .image("missing depth", ImageDesc::depth32(8, 8))
        .unwrap();
    let (mut graph, output) = pipeline
        .build(
            graph,
            FrameInputs {
                image_sources: &[
                    FrameImageSource {
                        source: ImageSource::Reuse(uninitialized),
                        ..FRESH_A
                    },
                    FRESH_B,
                ],
                views: &[VIEW_B],
                ..FrameInputs::default()
            },
        )
        .unwrap();
    graph.export_image(output.image()).unwrap();
    assert_eq!(
        graph.compile().err().unwrap().kind(),
        GraphErrorKind::UninitializedRead
    );
}

#[test]
fn preparation_rejects_an_unrelated_or_incomplete_producer_and_a_second_writer() {
    for (yaml, expected) in [
        (PIPELINE.replace("render: shadow_a", "render: missing"), "references unknown producer"),
        (PIPELINE.replace("render: shadow_a", "render: shadow_b"), "must clear and store only"),
        (PIPELINE.replace("load: clear, clear: 1", "load: load"), "must clear and store only"),
        (PIPELINE.replace("store: store", "store: discard"), "must clear and store only"),
        (PIPELINE.replace("  - id: shadow_b\n", "  - id: second_writer\n    kind: raster\n    shader: shadow\n    draw_list: casters_a\n    depth: {image: cascade_a, load: clear, clear: 1}\n  - id: shadow_b\n"), "also writes selectable image"),
    ] {
        let error = prepare(&yaml).err().unwrap();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn preparation_rejects_conditional_producers_and_incompatible_resolve_writes() {
    let conditional = COLOR_PIPELINE.replace(
        "    draw_list: meshes",
        "    draw_list: meshes\n    when: {draw_list: meshes, empty: false}",
    );
    let resolves_into_selectable = COLOR_PIPELINE.replace(
        "output: cached",
        "  - id: other_writer\n    kind: attachment\n    colors: [{image: msaa, load: clear, clear: [0, 0, 0, 1], resolve: cached}]\noutput: cached",
    );
    let resolves_into_other = COLOR_PIPELINE
        .replace("height: 8, samples: 4", "height: 8")
        .replace("source: {render: draw", "samples: 4, source: {render: draw")
        .replace("store: store}]", "store: store, resolve: msaa}]");
    for (yaml, expected) in [
        (conditional, "unconditional Raster pass"),
        (resolves_into_selectable, "also writes selectable image"),
        (resolves_into_other, "must clear and store only"),
    ] {
        let error = prepare(&yaml).err().unwrap();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn batched_raster_consumers_read_the_selected_image_after_its_producer() {
    let consumer = "  - id: consume
    kind: raster
    colors: [{image: mapped, load: clear, clear: [0, 0, 0, 1]}]
    batches:
      - shader: tone
        constants: [1073741824]
        bindings:
          - {group: 0, binding: 0, kind: image, resource: cached}
          - {group: 0, binding: 1, kind: sampler, sampler: nearest_clamp}
";
    let misplaced = COLOR_PIPELINE.replace("passes:\n", &format!("passes:\n{consumer}"));
    let error = prepare(&misplaced).err().unwrap();
    assert!(
        error.to_string().contains("reads selectable image"),
        "{error}"
    );

    let yaml = COLOR_PIPELINE.replace("output: cached", &format!("{consumer}output: mapped"));
    let mut pipeline = prepare(&yaml).unwrap();
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let cached = graph.image("cached", ImageDesc::rgba8(8, 8)).unwrap();
    graph
        .upload_image("prior color", cached, &[0; 256])
        .unwrap();
    let (mut graph, output) = pipeline
        .build(
            graph,
            FrameInputs {
                image_sources: &[FrameImageSource {
                    name: "cached",
                    source: ImageSource::Reuse(cached),
                }],
                ..FrameInputs::default()
            },
        )
        .unwrap();
    graph.export_image(output.image()).unwrap();
    assert!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .eq(["prior color", "consume"])
    );
}

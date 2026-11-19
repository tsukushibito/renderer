use renderer::graph::{
    BufferDesc, GraphCapacity, GraphPool, ImageDesc, RasterDraw, RasterParameters,
};
use renderer::program::ManagedRasterProgram;
use renderer_pipeline::{
    DrawCandidate, FrameInputs, FrameMaterial, FrameResource, FrameView, PrepareOptions,
    PreparedPipeline, Program,
};

const PIPELINE: &str = "
version: 1
resources:
  - {id: output, kind: image, source: external, width: 8, height: 4, format: rgba8_unorm}
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
            frames_in_flight: 1,
            max_draws_per_list: 1,
            max_materials: 0,
        },
        |_| Ok(Program::Raster(ManagedRasterProgram::mesh())),
    )
    .unwrap();
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
fn draw_list_overflow_returns_capacity_error_before_capturing_draws() {
    let mut pipeline = PreparedPipeline::from_yaml(
        PIPELINE,
        PrepareOptions {
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

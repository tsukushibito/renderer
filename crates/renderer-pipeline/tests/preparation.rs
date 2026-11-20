use std::num::NonZeroUsize;

use renderer::capacity::{GraphRequirements, Preparation};
use renderer::graph::{GraphCapacity, GraphCapacityKind, GraphErrorKind, GraphPool, ImageDesc};
use renderer_pipeline::{
    passes::output::{PreparedOutput, SdrEncoding, SdrOutput},
    unlit_forward::{ForwardConfig, PreparedForward},
};

#[test]
fn prepared_recipes_fit_their_declaration_budget_in_a_shared_graph() {
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    let forward = PreparedForward::prepare(
        &mut preparation,
        "forward",
        ForwardConfig {
            color: ImageDesc::rgba16_float(4, 3),
            clear_color: [0.3, 0.6, 1.2, 1.0],
            depth: true,
        },
        0,
    )
    .unwrap();
    let output = PreparedOutput::prepare(
        &mut preparation,
        [4, 3],
        SdrOutput {
            encoding: SdrEncoding::Linear,
            fxaa: true,
        },
    )
    .unwrap();
    preparation
        .add_graph_requirements(GraphRequirements {
            passes: 1,
            accesses: 1,
            name_bytes: "capture".len(),
            ..GraphRequirements::default()
        })
        .unwrap();
    let capacity = preparation
        .graph_capacity(GraphCapacity {
            resources: 0,
            passes: 0,
            accesses: 0,
            draws: 0,
            raster_constant_words: Some(0),
            bindings: 0,
            attachments: 0,
            name_bytes: 0,
            ..GraphCapacity::default()
        })
        .unwrap();
    let mut pool = GraphPool::new(capacity, 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let scene = forward.add_passes(&mut graph, &[]).unwrap();
    let hdr = graph.image_view(scene.color, 0, 0).unwrap();
    let images = output.add_passes(&mut graph, hdr, 2.0).unwrap();
    assert_eq!(
        graph.image_desc(images.display_linear).unwrap(),
        ImageDesc::rgba16_float(4, 3)
    );
    graph.readback_image("capture", images.sdr).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["forward", "tone_map", "write_sdr", "capture"]
    );

    let mut undersized = GraphPool::new(
        GraphCapacity {
            resources: 1,
            ..capacity
        },
        1,
    )
    .unwrap();
    let mut graph = undersized.begin().unwrap();
    let error = forward.add_passes(&mut graph, &[]).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
    assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Resources);
    drop(graph);
    assert!(undersized.begin().is_ok());
}

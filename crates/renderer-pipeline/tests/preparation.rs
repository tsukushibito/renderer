use std::num::NonZeroUsize;

use renderer::capacity::{ResourceClass, WorkingSet};
use renderer::graph::{
    BufferDesc, GraphCapacity, GraphCapacityKind, GraphErrorKind, GraphPool, ImageDesc, SampleCount,
};
use renderer::program::{ManagedRasterProgram, RasterPipelineRequest};
use renderer_pipeline::standard::{
    graphs::{ForwardConfig, PreparedForward, PreparedOutput},
    passes::{SdrEncoding, SdrOutput},
};
use renderer_pipeline::{GraphRequirements, Preparation};

#[test]
fn physical_reservations_include_concurrency_and_share_exact_raster_variants() {
    let mut preparation = Preparation::new(NonZeroUsize::new(2).unwrap());
    let color = ImageDesc::rgba16_float(4, 3);
    let globals = [
        ResourceClass::Image {
            desc: color,
            count: 3,
        },
        ResourceClass::Buffer {
            desc: BufferDesc { size: 32 },
            count: 4,
        },
    ];
    let program = ManagedRasterProgram::mesh_hdr();
    let raster = [
        program.clone().into(),
        program.clone().into(),
        RasterPipelineRequest {
            program,
            sample_count: SampleCount::X4,
        },
    ];
    preparation.reserve_image(color, 1).unwrap();
    preparation
        .reserve_buffer(BufferDesc { size: 32 }, 2)
        .unwrap();
    preparation
        .add_working_set(WorkingSet {
            resources: &globals,
            raster: &raster,
            compute: &[],
        })
        .unwrap();

    let set = preparation.working_set();
    assert!(matches!(
        set.resources,
        [ResourceClass::Image { desc, count: 5 }, ResourceClass::Buffer { count: 8, .. }]
            if *desc == color
    ));
    assert_eq!(set.raster.len(), 2);
    assert_eq!(set.raster[0].sample_count, SampleCount::X1);
    assert_eq!(set.raster[1].sample_count, SampleCount::X4);
}

#[test]
fn invalid_reservations_leave_preparation_available_for_retry() {
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    let hdr = ImageDesc::rgba16_float(4, 3);
    preparation.reserve_image(hdr, usize::MAX).unwrap();
    let failed = [
        ResourceClass::Buffer {
            desc: BufferDesc { size: 8 },
            count: 1,
        },
        ResourceClass::Image {
            desc: hdr,
            count: 1,
        },
    ];
    assert!(
        preparation
            .add_working_set(WorkingSet {
                resources: &failed,
                ..Default::default()
            })
            .is_err()
    );
    assert_eq!(preparation.working_set().resources.len(), 1);

    assert!(
        PreparedOutput::prepare(
            &mut preparation,
            [4, 3],
            SdrOutput {
                encoding: SdrEncoding::Srgb,
                fxaa: false
            },
        )
        .is_err()
    );
    assert!(preparation.working_set().raster.is_empty());
    assert_eq!(
        preparation.graph_requirements(),
        GraphRequirements::default()
    );

    preparation
        .reserve_buffer(BufferDesc { size: 8 }, 2)
        .unwrap();
    assert!(matches!(
        preparation.working_set().resources[1],
        ResourceClass::Buffer { count: 2, .. }
    ));
}

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

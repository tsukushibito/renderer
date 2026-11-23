use std::num::NonZeroUsize;

use renderer::{
    passes::output::{PreparedOutput, SdrOutput},
    pbr::VIEW_BYTES,
    pbr_deferred::passes as deferred,
    unlit_forward::{ForwardConfig, PreparedForward},
};
use renderer_core::capacity::{GraphRequirements, Preparation};
use renderer_core::graph::{
    Attachment, Binding, BindingResource, BufferDesc, GraphCapacity, GraphCapacityKind,
    GraphErrorKind, GraphPool, ImageDesc, ImageView, LoadOp, RasterAttachments, StoreOp,
};
use renderer_core::program::SamplerDescriptor;

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
    let output =
        PreparedOutput::prepare(&mut preparation, [4, 3], SdrOutput::display(true)).unwrap();
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
    let display = images.sdr.display().unwrap();
    assert!(images.sdr.capture().is_none());
    graph
        .readback_image("capture", display.raw_image())
        .unwrap();
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

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn deferred_lighting(environment: ImageView, shadow: ImageView) -> [Binding; 10] {
    let image = |group, binding, view| Binding {
        group,
        binding,
        resource: BindingResource::ImageView(view),
    };
    let sampler = |group, binding| Binding {
        group,
        binding,
        resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
    };
    [
        image(2, 0, environment),
        image(2, 1, environment),
        image(2, 2, environment),
        sampler(2, 3),
        image(2, 4, environment),
        image(3, 0, shadow),
        image(3, 1, shadow),
        image(3, 2, shadow),
        image(3, 3, shadow),
        sampler(3, 4),
    ]
}

#[test]
fn deferred_pass_fits_its_advertised_reservations() {
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    deferred::reserve(&mut preparation, [4, 3]).unwrap();
    assert_eq!(preparation.working_set().raster.len(), 3);
    preparation
        .add_graph_requirements(GraphRequirements {
            resources: 4,
            passes: 2,
            accesses: 2,
            attachments: 1,
            name_bytes: "hdr".len()
                + "depth".len()
                + "camera".len()
                + "upload camera".len()
                + "environment".len()
                + "clear environment".len(),
            upload_bytes: VIEW_BYTES,
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
            upload_bytes: 0,
            ..GraphCapacity::default()
        })
        .unwrap();
    let mut pool = GraphPool::new(capacity, 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let hdr_desc = ImageDesc::rgba16_float(4, 3);
    let hdr = graph.image("hdr", hdr_desc).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(4, 3)).unwrap();
    let environment = graph.image("environment", hdr_desc).unwrap();
    let camera = graph
        .buffer(
            "camera",
            BufferDesc {
                size: VIEW_BYTES as u64,
            },
        )
        .unwrap();
    graph
        .upload_buffer("upload camera", camera, 0, &[0; VIEW_BYTES])
        .unwrap();
    let hdr_view = graph.image_view(hdr, 0, 0).unwrap();
    let depth_view = graph.image_view(depth, 0, 0).unwrap();
    let environment = graph.image_view(environment, 0, 0).unwrap();
    graph
        .attachment_pass(
            "clear environment",
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: environment,
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                }],
                depth: None,
            },
        )
        .unwrap();
    let planes = deferred::append(
        &mut graph,
        deferred::Inputs {
            local_lights: Some((environment, environment)),
            hdr: hdr_view,
            depth: depth_view,
            clear: [0.0; 4],
            camera,
            view_projection: IDENTITY,
            lighting: deferred_lighting(environment, depth_view),
        },
        |_, _| Ok(()),
    )
    .unwrap();
    for plane in [
        planes.base_color_metallic,
        planes.normals_roughness,
        planes.emissive_ao,
    ] {
        assert_eq!(graph.image_desc(plane).unwrap(), hdr_desc);
    }
    graph.export_image(hdr).unwrap();
    assert_eq!(graph.compile().unwrap().pass_count(), 5);
}

#[test]
fn deferred_pass_rejects_invalid_targets_before_consuming_graph_capacity() {
    let hdr = ImageDesc::rgba16_float(4, 4);
    for (hdr_desc, mip, depth_desc) in [
        (
            ImageDesc {
                mip_levels: 2,
                ..hdr
            },
            1,
            ImageDesc::depth32(2, 2),
        ),
        (hdr, 0, ImageDesc::depth32(2, 2)),
    ] {
        let mut pool = GraphPool::new(
            GraphCapacity {
                resources: 4,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let hdr = graph.image("hdr", hdr_desc).unwrap();
        let depth = graph.image("depth", depth_desc).unwrap();
        let hdr = graph.image_view(hdr, mip, 0).unwrap();
        let depth = graph.image_view(depth, 0, 0).unwrap();
        let camera = graph
            .buffer(
                "camera",
                BufferDesc {
                    size: VIEW_BYTES as u64,
                },
            )
            .unwrap();
        let error = deferred::append(
            &mut graph,
            deferred::Inputs {
                local_lights: None,
                hdr,
                depth,
                clear: [0.0; 4],
                camera,
                view_projection: IDENTITY,
                lighting: deferred_lighting(hdr, depth),
            },
            |_, _| panic!("invalid targets must be rejected before recording geometry"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        graph.image("remaining slot", hdr_desc).unwrap();
    }
}

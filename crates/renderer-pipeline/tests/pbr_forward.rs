use std::num::NonZeroUsize;

use renderer::capacity::{GraphRequirements, Preparation};
use renderer::geometry::GeometryData;
use renderer::graph::{
    Attachment, BufferDesc, Graph, GraphCapacity, GraphCapacityKind, GraphErrorKind, GraphPool,
    ImageDesc, LoadOp, RasterAttachments, StoreOp,
};
use renderer_pipeline::passes::output::{SdrEncoding, SdrOutput};
use renderer_pipeline::pbr_forward::{
    Bounds, Camera, Config, Draw, Environment, Frame, Material, MaterialFactors, Prepared,
    PreparedView, SceneLimits, ShadowSettings, ShadowSource, Sun, prepare,
};

const EXTENT: [u32; 2] = [7, 5];
const SHADOW_SIZE: u32 = 16;

fn recipe() -> Prepared {
    prepare(
        Config {
            extent: EXTENT,
            clear_color: [0.1, 0.2, 0.3, 1.0],
            shadows: ShadowSettings {
                resolution: SHADOW_SIZE,
                ..Default::default()
            },
            output: SdrOutput {
                encoding: SdrEncoding::Srgb,
                fxaa: false,
            },
        },
        SceneLimits {
            frames_in_flight: NonZeroUsize::MIN,
            max_draws: 2,
            max_materials: 1,
            max_casters_per_cascade: 1,
        },
    )
    .unwrap()
}

fn graph_pool(recipe: &Prepared) -> GraphPool {
    let recipe = recipe.preparation();
    let mut all = Preparation::new(NonZeroUsize::MIN);
    all.add_requirements(recipe.graph_requirements(), recipe.working_set())
        .unwrap();
    all.add_graph_requirements(GraphRequirements {
        resources: 9,
        passes: 12,
        accesses: 15,
        attachments: 1,
        name_bytes: 256,
        ..GraphRequirements::default()
    })
    .unwrap();
    let limits = all
        .graph_capacity(GraphCapacity {
            resources: 0,
            passes: 0,
            accesses: 0,
            draws: 0,
            raster_constant_words: Some(0),
            attachments: 0,
            bindings: 0,
            name_bytes: 0,
            ..GraphCapacity::default()
        })
        .unwrap();
    GraphPool::new(limits, 1).unwrap()
}

fn inputs(recipe: &Prepared, graph: &mut Graph) -> (PreparedView, Material, Draw) {
    let geometry = GeometryData::new(48, &[0; 144], &[0, 1, 2]).unwrap();
    let vertices = graph
        .buffer(
            "vertices",
            BufferDesc {
                size: geometry.vertex_byte_len(),
            },
        )
        .unwrap();
    let indices = graph
        .buffer(
            "indices",
            BufferDesc {
                size: geometry.index_byte_len(),
            },
        )
        .unwrap();
    geometry
        .upload_vertices(graph, "upload vertices", vertices)
        .unwrap();
    geometry
        .upload_indices(graph, "upload indices", indices)
        .unwrap();
    let factors = graph.buffer("factors", BufferDesc { size: 64 }).unwrap();
    graph
        .upload_buffer(
            "upload factors",
            factors,
            0,
            &MaterialFactors::default().encode().unwrap(),
        )
        .unwrap();

    let color = graph.image("color", ImageDesc::rgba8_srgb(1, 1)).unwrap();
    graph
        .upload_image("upload color", color, &[255; 4])
        .unwrap();
    let color = graph.image_view(color, 0, 0).unwrap();
    let linear = graph.image("linear", ImageDesc::rgba8(1, 1)).unwrap();
    graph
        .upload_image("upload linear", linear, &[127; 4])
        .unwrap();
    let linear = graph.image_view(linear, 0, 0).unwrap();
    let environment = graph
        .image("environment", ImageDesc::rgba16_float(1, 1))
        .unwrap();
    graph
        .upload_image("upload environment", environment, &[0; 8])
        .unwrap();
    let environment = graph.image_view(environment, 0, 0).unwrap();
    let view = recipe
        .prepare_view(
            Camera {
                position: [0.0, 0.0, 4.0],
                target: [0.0; 3],
                fov_y_degrees: 55.0,
                near: 0.1,
                far: 40.0,
            },
            Sun::new([0.4, 1.0, 0.3], [1.0; 3]).unwrap(),
            Bounds::new([-1.0; 3], [1.0; 3]).unwrap(),
            Environment {
                irradiance: environment,
                reflection: environment,
                brdf_lut: environment,
                strength: 1.0,
                yaw_radians: 0.0,
                max_prefilter_lod: 0,
            },
        )
        .unwrap();
    let material = Material {
        factors,
        factor_offset: 0,
        textures: [color, linear, linear, color],
    };
    let draw = Draw {
        vertices,
        indices,
        first_index: 0,
        index_count: geometry.index_count(),
        material: 0,
    };
    (view, material, draw)
}

#[test]
fn maximum_frame_can_reuse_one_cascade_and_insert_an_hdr_pass() {
    let mut recipe = recipe();
    let mut pool = graph_pool(&recipe);
    let mut graph = pool.begin().unwrap();
    let (view, material, draw) = inputs(&recipe, &mut graph);
    let shadow_desc = ImageDesc::depth32(SHADOW_SIZE, SHADOW_SIZE);
    let reused = graph.image("cached cascade", shadow_desc).unwrap();
    let reused_view = graph.image_view(reused, 0, 0).unwrap();
    graph
        .attachment_pass(
            "cached depth producer",
            RasterAttachments {
                colors: &[],
                color_resolves: &[],
                depth: Some(Attachment {
                    view: reused_view,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    let redraw = graph.image("redraw target", shadow_desc).unwrap();
    let materials = [material];
    let draws = [draw];
    let mut frame = Frame::new(&view, &materials, &draws, &draws, &draws);
    frame.shadows[0] = ShadowSource::Render {
        target: Some(reused),
        casters: &draws,
    };
    frame.shadows[2] = ShadowSource::Reuse(reused);
    let error = recipe.add_scene(&mut graph, frame).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    let mut frame = Frame::new(&view, &materials, &draws, &draws, &draws);
    frame.shadows[1] = ShadowSource::Render {
        target: Some(redraw),
        casters: &draws,
    };
    frame.shadows[2] = ShadowSource::Reuse(reused);
    let scene = recipe.add_scene(&mut graph, frame).unwrap();
    assert_eq!(scene.shadows[1], redraw);
    assert_eq!(scene.shadows[2], reused);
    assert_eq!(
        graph.image_desc(scene.depth).unwrap(),
        ImageDesc::depth32(7, 5)
    );
    let custom = graph
        .image("custom HDR", ImageDesc::rgba16_float(7, 5))
        .unwrap();
    graph
        .copy_image("custom HDR pass", scene.hdr, custom)
        .unwrap();
    let output = recipe.add_output(&mut graph, custom, 1.0).unwrap();
    graph.readback_image("capture", output.sdr).unwrap();
    let graph = graph.compile().unwrap();
    let names: Vec<_> = graph.ordered_pass_names().collect();
    for name in [
        "shadow_0",
        "shadow_1",
        "shadow_3",
        "cached depth producer",
        "scene_transparent",
    ] {
        assert!(names.contains(&name), "missing pass {name}");
    }
    assert!(!names.contains(&"shadow_2"));
    let position = |name| names.iter().position(|&pass| pass == name).unwrap();
    assert!(position("custom HDR pass") < position("tone_map"));
    assert!(position("tone_map") < position("write_sdr"));
}

#[test]
fn rejected_workload_does_not_consume_graph_or_recipe_scratch() {
    let mut recipe = recipe();
    let mut pool = graph_pool(&recipe);
    for casters in [true, false] {
        let mut graph = pool.begin().unwrap();
        let (view, material, draw) = inputs(&recipe, &mut graph);
        let draws = [draw; 3];
        let materials = [material];
        let overflow = if casters {
            Frame::new(&view, &materials, &draws[..1], &[], &draws[..2])
        } else {
            Frame::new(&view, &materials, &draws, &[], &draws[..1])
        };
        let error = recipe.add_passes(&mut graph, overflow).unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
        let bounds = error.capacity().unwrap();
        assert_eq!(bounds.kind, GraphCapacityKind::Draws);
        assert_eq!(bounds.required, Some(if casters { 2 } else { 3 }));
        let valid = Frame::new(&view, &materials, &draws[..1], &[], &draws[..1]);
        let output = recipe.add_passes(&mut graph, valid).unwrap();
        graph.readback_image("capture", output.sdr).unwrap();
        assert!(graph.compile().is_ok());
    }
}

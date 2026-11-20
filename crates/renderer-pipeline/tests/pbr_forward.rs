use std::num::NonZeroUsize;

use renderer::capacity::{GraphRequirements, Preparation, ResourceClass};
use renderer::geometry::GeometryData;
use renderer::graph::{
    Attachment, BufferDesc, Graph, GraphCapacity, GraphCapacityKind, GraphErrorKind, GraphPool,
    ImageDesc, IndexedIndirectTable, IndexedIndirectTemplate, LoadOp, RasterAttachments,
    SampleCount, StoreOp,
};
use renderer::program::SamplerDescriptor;
use renderer_pipeline::passes::output::SdrOutput;
use renderer_pipeline::pbr::{
    self, AlphaMode, Bounds, CASCADE_COUNT, CameraStorage, Defaults, Draw, Environment, Frame,
    FrameShadows, IndexedDraw, Material, MaterialFactors, MaterialTextures, MaterialUniform,
    PreparedView, Projection, SceneLimits, ShadowHistory, ShadowKey, ShadowSettings, ShadowSource,
    Sun, Transform, VIEW_BYTES, ViewInput,
};
use renderer_pipeline::pbr_forward::{
    Config, Prepared, Programs, add_shadow_pass, prepare, prepare_with_programs,
};

const EXTENT: [u32; 2] = [7, 5];
const SHADOW_SIZE: u32 = 16;

fn config() -> Config {
    Config {
        extent: EXTENT,
        clear_color: [0.1, 0.2, 0.3, 1.0],
        shadows: Some(ShadowSettings {
            resolution: SHADOW_SIZE,
            ..Default::default()
        }),
        output: Some(SdrOutput::capture(false)),
        samples: SampleCount::X1,
        camera_storage: CameraStorage::Transient,
    }
}

fn limits() -> SceneLimits {
    SceneLimits {
        frames_in_flight: NonZeroUsize::MIN,
        max_draws: 2,
        max_materials: 2,
        max_casters_per_cascade: 1,
        max_transforms: 0,
    }
}

fn recipe() -> Prepared {
    prepare(config(), limits()).unwrap()
}

fn graph_pool(recipe: &Prepared) -> GraphPool {
    let recipe = recipe.preparation();
    let mut all = Preparation::new(NonZeroUsize::MIN);
    all.add_requirements(recipe.graph_requirements(), recipe.working_set())
        .unwrap();
    all.add_graph_requirements(GraphRequirements {
        resources: 10,
        passes: 13,
        accesses: 16,
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

fn inputs(recipe: &Prepared, graph: &mut Graph) -> (PreparedView, Material, IndexedDraw) {
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
    let factors = MaterialUniform::upload(graph, "factors", MaterialFactors::default()).unwrap();

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
    let near = 0.1;
    let far = 40.0;
    let z_scale = far / (near - far);
    let scale = 1.0 / (55.0_f32.to_radians() * 0.5).tan();
    let aspect = EXTENT[0] as f32 / EXTENT[1] as f32;
    let input = ViewInput::new(
        [
            [scale / aspect, 0.0, 0.0, 0.0],
            [0.0, -scale, 0.0, 0.0],
            [0.0, 0.0, z_scale, -1.0],
            [0.0, 0.0, -4.0 * z_scale + near * far / (near - far), 4.0],
        ],
        [0.0, 0.0, 4.0],
        [0.0, 0.0, -1.0],
        [near, far],
        Projection::Perspective,
    )
    .unwrap();
    let view = recipe
        .prepare_view(
            input,
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
    let material = Material::new(
        factors,
        MaterialTextures {
            base_color: color,
            normal: linear,
            orm: linear,
            emissive: color,
        },
    );
    let draw = IndexedDraw {
        vertices,
        indices,
        first_index: 0,
        index_count: geometry.index_count(),
        material: 0,
    };
    (view, material, draw)
}

#[test]
fn disabled_shadows_omit_maps_and_reject_cascade_inputs() {
    let mut recipe = prepare(
        Config {
            shadows: None,
            ..config()
        },
        limits(),
    )
    .unwrap();
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    preparation
        .add_requirements(
            recipe.preparation().graph_requirements(),
            recipe.preparation().working_set(),
        )
        .unwrap();
    Defaults::Unshadowed.prepare(&mut preparation).unwrap();
    let depth_classes: Vec<_> = preparation
        .working_set()
        .resources
        .iter()
        .filter_map(|class| match class {
            ResourceClass::Image { desc, count }
                if desc.format == renderer::ImageFormat::Depth32Float =>
            {
                Some((*desc, *count))
            }
            _ => None,
        })
        .collect();
    assert_eq!(depth_classes.len(), 2);
    assert!(depth_classes.contains(&(ImageDesc::depth32(1, 1), 1)));
    assert!(depth_classes.contains(&(ImageDesc::depth32(7, 5), 1)));

    let mut pool = graph_pool(&recipe);
    let mut graph = pool.begin().unwrap();
    let (view, material, draw) = inputs(&recipe, &mut graph);
    let history = ShadowHistory::default();
    assert_eq!(
        history
            .begin_view(&mut graph, &view, [0; CASCADE_COUNT])
            .err()
            .unwrap()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    assert!(view.shadow_matrices().is_none());
    assert!(
        view.cascade_overlaps(0, Bounds::new([0.0; 3], [0.0; 3]).unwrap())
            .is_none()
    );
    let materials = [material];
    let draws = [Draw::Indexed(draw)];
    let error = recipe
        .add_scene(
            &mut graph,
            Frame::new(&view, &materials, &draws, &[], &draws),
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);

    let defaults = Defaults::Unshadowed.upload(&mut graph).unwrap();
    let scene = recipe
        .add_scene(
            &mut graph,
            Frame::with_defaults(&view, &materials, &draws, &[], &draws, defaults).unwrap(),
        )
        .unwrap();
    assert!(scene.shadows.is_none());
    graph
        .readback_image("capture unshadowed HDR", scene.hdr)
        .unwrap();
    let graph = graph.compile().unwrap();
    let names: Vec<_> = graph.ordered_pass_names().collect();
    assert!(names.contains(&"initialize disabled shadows"));
    assert!(names.iter().all(|name| !name.starts_with("shadow_")));
}

#[test]
fn standard_techniques_share_output_and_reject_unsupported_lights() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let lights = [pbr::PointLight {
        position: [0.0, 0.0, 1.0],
        radius: 2.0,
        color: [1.0; 3],
        intensity: 1.0,
    }];
    for technique in [
        pbr::Technique::Forward,
        pbr::Technique::ForwardPlus,
        pbr::Technique::Deferred,
    ] {
        let mut prepared = pbr::Prepared::new(technique, config(), limits()).unwrap();
        let mut graph = pool.begin().unwrap();
        let (view, material, draw) = inputs(&recipe(), &mut graph);
        let materials = [material.with_sampler(SamplerDescriptor::NearestClamp)];
        let draws = [Draw::Indexed(draw)];
        let frame = || Frame::new(&view, &materials, &draws, &[], &draws);
        let supported_lights = if technique.supports_point_lights() {
            lights.as_slice()
        } else {
            let error = prepared
                .add_scene(&mut graph, frame(), &lights)
                .unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
            &[]
        };
        let images = prepared
            .add_passes(&mut graph, frame(), supported_lights)
            .unwrap();
        assert_eq!(images.gbuffer.is_some(), technique.provides_gbuffer());
        assert_eq!(
            images.depth.is_some(),
            technique.provides_scene_depth(SampleCount::X1)
        );
        if let Some(gbuffer) = images.gbuffer {
            assert_eq!(Some(gbuffer.depth), images.depth);
        }
        images
            .sdr
            .capture()
            .unwrap()
            .readback(&mut graph, "test output")
            .unwrap();
        let graph = graph.compile().unwrap();
        let names = || graph.ordered_pass_names();
        assert_eq!(
            names().any(|name| name == "Forward+ tile light masks"),
            technique.supports_point_lights(),
        );
        assert_eq!(
            names().filter(|name| *name == "pbr camera upload").count(),
            1
        );
    }
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
    let factors = MaterialUniform::upload(
        &mut graph,
        "blend factors",
        MaterialFactors {
            alpha_mode: AlphaMode::Blend,
            ..MaterialFactors::default()
        },
    )
    .unwrap();
    let materials = [material, Material::new(factors, material.textures())];
    let draws = [Draw::Indexed(draw)];
    let transparent = [Draw::Indexed(IndexedDraw {
        material: 1,
        ..draw
    })];
    let mut frame = Frame::new(&view, &materials, &draws, &transparent, &draws);
    let FrameShadows::Cascades(cascades) = &mut frame.shadows else {
        unreachable!()
    };
    cascades[0] = ShadowSource::Render {
        target: Some(reused),
        casters: &draws,
    };
    cascades[2] = ShadowSource::Reuse(reused);
    let error = recipe.add_scene(&mut graph, frame).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    let mut frame = Frame::new(&view, &materials, &draws, &transparent, &draws);
    let FrameShadows::Cascades(cascades) = &mut frame.shadows else {
        unreachable!()
    };
    cascades[1] = ShadowSource::Render {
        target: Some(redraw),
        casters: &draws,
    };
    cascades[2] = ShadowSource::Reuse(reused);
    let scene = recipe.add_scene(&mut graph, frame).unwrap();
    let shadows = scene.shadows.unwrap();
    assert_eq!(shadows[1], redraw);
    assert_eq!(shadows[2], reused);
    assert_eq!(
        graph.image_desc(scene.depth.unwrap()).unwrap(),
        ImageDesc::depth32(7, 5)
    );
    let custom = graph
        .image("custom HDR", ImageDesc::rgba16_float(7, 5))
        .unwrap();
    graph
        .copy_image("custom HDR pass", scene.hdr, custom)
        .unwrap();
    let output = recipe.add_output(&mut graph, custom, 1.0).unwrap();
    output
        .sdr
        .capture()
        .unwrap()
        .readback(&mut graph, "capture")
        .unwrap();
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
fn abandoned_shadow_frame_releases_history_without_publishing() {
    let history = ShadowHistory::default();
    let mut pool = GraphPool::new(GraphCapacity::default(), 2).unwrap();
    let mut graph = pool.begin().unwrap();
    let mut other = pool.begin().unwrap();
    let keys = [ShadowKey::new([[0.0; 4]; 4], 0); CASCADE_COUNT];

    let frame = history.begin_frame(&mut graph, SHADOW_SIZE, keys).unwrap();
    assert!(history.begin_frame(&mut other, SHADOW_SIZE, keys).is_err());
    assert!(frame.export(&mut other).is_err());

    let frame = history.begin_frame(&mut other, SHADOW_SIZE, keys).unwrap();
    for cascade in 0..CASCADE_COUNT {
        assert!(matches!(
            frame.source(cascade, &[]),
            ShadowSource::Render { target: Some(target), casters: [] } if target == frame.target(cascade)
        ));
    }
    let updates = frame.export(&mut other).unwrap();
    assert_eq!(updates.reused(), [false; CASCADE_COUNT]);
    assert!(
        other.compile().is_err(),
        "an undrawn export has no valid contents"
    );
    drop(updates);

    let mut next = pool.begin().unwrap();
    let frame = history.begin_frame(&mut next, SHADOW_SIZE, keys).unwrap();
    for cascade in 0..CASCADE_COUNT {
        let view = next.image_view(frame.target(cascade), 0, 0).unwrap();
        add_shadow_pass(&mut next, cascade, view, |_| Ok(())).unwrap();
    }
    let updates = frame.export(&mut next).unwrap();
    assert_eq!(updates.reused(), [false; CASCADE_COUNT]);
    let plan = next.compile().unwrap();
    assert_eq!(plan.ordered_pass_names().count(), CASCADE_COUNT);
}

#[test]
fn rejected_workload_does_not_consume_graph_or_recipe_scratch() {
    let mut recipe = recipe();
    let mut pool = graph_pool(&recipe);
    for casters in [true, false] {
        let mut graph = pool.begin().unwrap();
        let (view, material, draw) = inputs(&recipe, &mut graph);
        let draws = [Draw::Indexed(draw); 3];
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
        output
            .sdr
            .capture()
            .unwrap()
            .readback(&mut graph, "capture")
            .unwrap();
        assert!(graph.compile().is_ok());
    }
}

#[test]
fn hdr_only_preparation_does_not_reserve_output_images_or_programs() {
    let standard = recipe();
    let mut hdr_only = prepare(
        Config {
            output: None,
            ..config()
        },
        limits(),
    )
    .unwrap();
    let full_requirements = standard.preparation().graph_requirements();
    let hdr_requirements = hdr_only.preparation().graph_requirements();
    assert_eq!(full_requirements.resources, hdr_requirements.resources + 2);
    assert_eq!(full_requirements.passes, hdr_requirements.passes + 2);
    let full_programs = standard.preparation().working_set().raster;
    let hdr_programs = hdr_only.preparation().working_set().raster;
    assert_eq!(full_programs.len(), hdr_programs.len() + 2);

    let mut pool = graph_pool(&hdr_only);
    let mut graph = pool.begin().unwrap();
    let (view, _, _) = inputs(&hdr_only, &mut graph);
    let frame = Frame::new(&view, &[], &[], &[], &[]);
    let error = hdr_only.add_passes(&mut graph, frame).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    let scene = hdr_only
        .add_scene(&mut graph, Frame::new(&view, &[], &[], &[], &[]))
        .unwrap();
    graph.readback_image("HDR", scene.hdr).unwrap();
    graph.readback_image("depth", scene.depth.unwrap()).unwrap();
    let plan = graph.compile().unwrap();
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        ["scene_opaque", "HDR", "depth"]
    );
}

#[test]
fn material_alpha_routes_to_the_matching_pass_before_graph_mutation() {
    let mut recipe = recipe();
    let mut pool = graph_pool(&recipe);
    let mut graph = pool.begin().unwrap();
    let (view, opaque, draw) = inputs(&recipe, &mut graph);
    let encoded = MaterialFactors {
        alpha_mode: AlphaMode::Blend,
        ..MaterialFactors::default()
    }
    .encode()
    .unwrap();
    let factors = MaterialUniform::upload_encoded(&mut graph, "blend factors", &encoded).unwrap();
    let materials = [opaque, Material::new(factors, opaque.textures())];
    let opaque = [Draw::Indexed(draw)];
    let blended = [Draw::Indexed(IndexedDraw {
        material: 1,
        ..draw
    })];
    for (frame, pass, material) in [
        (
            Frame::new(&view, &materials, &blended, &[], &[]),
            "scene_opaque",
            1,
        ),
        (
            Frame::new(&view, &materials, &[], &opaque, &[]),
            "scene_transparent",
            0,
        ),
        (
            Frame::new(&view, &materials, &[], &[], &blended),
            "shadow_0",
            1,
        ),
    ] {
        let error = recipe.add_scene(&mut graph, frame).unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some(pass));
        assert!(
            error
                .message
                .contains(&format!("draw 0 uses material {material}"))
        );
    }
    let frame = Frame::new(&view, &materials, &opaque, &blended, &opaque);
    let result = recipe.add_passes(&mut graph, frame).unwrap();
    result
        .sdr
        .capture()
        .unwrap()
        .readback(&mut graph, "capture")
        .unwrap();
    let compiled = graph.compile().unwrap();
    assert_eq!(
        compiled
            .ordered_pass_names()
            .filter(|&name| name == "pbr camera upload")
            .count(),
        1,
    );
}

#[test]
fn invalid_exposure_is_rejected_before_scene_declarations() {
    let mut recipe = recipe();
    let mut pool = graph_pool(&recipe);
    let mut graph = pool.begin().unwrap();
    let (view, material, draw) = inputs(&recipe, &mut graph);
    let materials = [material];
    let draws = [Draw::Indexed(draw)];
    let mut frame = Frame::new(&view, &materials, &draws, &[], &[]);
    frame.exposure = f32::NAN;
    let error = recipe.add_passes(&mut graph, frame).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.pass_name(), Some("tone_map"));
    let output = recipe
        .add_passes(&mut graph, Frame::new(&view, &materials, &draws, &[], &[]))
        .unwrap();
    output
        .sdr
        .capture()
        .unwrap()
        .readback(&mut graph, "read SDR")
        .unwrap();
    let plan = graph.compile().unwrap();
    assert!(plan.ordered_pass_names().any(|name| name == "write_sdr"));
}

#[test]
fn msaa_resolves_empty_opaque_batches_and_transparent_batches_without_depth_output() {
    let mut recipe = prepare(
        Config {
            samples: SampleCount::X4,
            ..config()
        },
        limits(),
    )
    .unwrap();
    let mut pool = graph_pool(&recipe);
    for transparent in [false, true] {
        let mut graph = pool.begin().unwrap();
        let (view, material, draw) = inputs(&recipe, &mut graph);
        let factors = MaterialUniform::upload(
            &mut graph,
            "blend factors",
            MaterialFactors {
                alpha_mode: AlphaMode::Blend,
                ..MaterialFactors::default()
            },
        )
        .unwrap();
        let materials = [Material::new(factors, material.textures())];
        let draws = [Draw::Indexed(draw)];
        let frame = Frame::new(
            &view,
            &materials,
            &[],
            if transparent { &draws } else { &[] },
            &[],
        );
        let output = recipe.add_passes(&mut graph, frame).unwrap();
        assert!(output.depth.is_none());
        assert_eq!(
            graph.image_desc(output.hdr).unwrap().sample_count,
            SampleCount::X1
        );
        output
            .sdr
            .capture()
            .unwrap()
            .readback(&mut graph, "capture")
            .unwrap();
        let graph = graph.compile().unwrap();
        let expected = if transparent {
            "scene_transparent_resolve"
        } else {
            "scene_opaque_resolve"
        };
        assert!(graph.ordered_pass_names().any(|name| name == expected));
    }
}

#[test]
fn external_camera_can_be_uploaded_once_before_the_recipe() {
    let mut recipe = prepare(
        Config {
            camera_storage: CameraStorage::PreUploaded,
            ..config()
        },
        limits(),
    )
    .unwrap();
    assert_eq!(recipe.preparation().graph_requirements().upload_bytes, 0);
    assert!(!recipe.preparation().working_set().resources.iter().any(
        |class| matches!(class, renderer::capacity::ResourceClass::Buffer { desc, .. } if desc.size == VIEW_BYTES as u64)
    ));
    let mut pool = graph_pool(&recipe);
    let mut graph = pool.begin().unwrap();
    let (view, material, draw) = inputs(&recipe, &mut graph);
    let camera = graph
        .buffer(
            "caller camera",
            BufferDesc {
                size: VIEW_BYTES as u64,
            },
        )
        .unwrap();
    graph
        .upload_buffer("caller view upload", camera, 0, view.uniform_bytes())
        .unwrap();
    let materials = [material];
    let draws = [Draw::Indexed(draw)];
    let mut frame = Frame::new(&view, &materials, &draws, &[], &draws);
    frame.camera_target = Some(camera);
    let output = recipe.add_passes(&mut graph, frame).unwrap();
    assert_eq!(output.camera, camera);
    output
        .sdr
        .capture()
        .unwrap()
        .readback(&mut graph, "capture")
        .unwrap();
    let graph = graph.compile().unwrap();
    assert!(
        graph
            .ordered_pass_names()
            .any(|name| name == "caller view upload")
    );
    assert!(
        !graph
            .ordered_pass_names()
            .any(|name| name == "pbr camera upload")
    );
}

#[test]
fn custom_programs_validate_their_role_during_preparation() {
    let programs = Programs {
        opaque: renderer_pipeline::passes::pbr::blend_program(),
        ..Programs::default()
    };
    assert_eq!(
        prepare_with_programs(config(), limits(), programs)
            .err()
            .unwrap()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    assert!(prepare_with_programs(config(), limits(), Programs::default()).is_ok());
}

#[test]
fn transformed_draws_check_ranges_before_appending_to_the_graph() {
    let mut recipe = prepare(
        config(),
        SceneLimits {
            max_transforms: 1,
            ..limits()
        },
    )
    .unwrap();
    let mut pool = graph_pool(&recipe);
    let mut graph = pool.begin().unwrap();
    let (view, material, draw) = inputs(&recipe, &mut graph);
    let materials = [material];
    let transforms = [Transform::IDENTITY];
    let invalid = [Draw::Instanced {
        geometry: draw,
        first_transform: 1,
        instance_count: 1,
    }];
    let mut frame = Frame::new(&view, &materials, &invalid, &[], &[]);
    frame.transforms = &transforms;
    assert_eq!(
        recipe.add_passes(&mut graph, frame).unwrap_err().kind(),
        GraphErrorKind::InvalidInput
    );

    let instanced = Draw::Instanced {
        geometry: draw,
        first_transform: 0,
        instance_count: 1,
    };
    let opaque = [Draw::Indexed(draw), instanced];
    let casters = [instanced];
    let mut frame = Frame::new(&view, &materials, &opaque, &[], &casters);
    frame.transforms = &transforms;
    let output = recipe.add_passes(&mut graph, frame).unwrap();
    output
        .sdr
        .capture()
        .unwrap()
        .readback(&mut graph, "capture")
        .unwrap();
    assert!(graph.compile().is_ok());
}

#[test]
fn opaque_indirect_group_uses_one_prepared_model_transform() {
    let mut recipe = prepare(
        config(),
        SceneLimits {
            max_transforms: 1,
            ..limits()
        },
    )
    .unwrap();
    let mut pool = graph_pool(&recipe);
    let mut graph = pool.begin().unwrap();
    let (view, material, draw) = inputs(&recipe, &mut graph);
    let table = IndexedIndirectTable::new(&[IndexedIndirectTemplate {
        first_index: 0,
        index_count: draw.index_count,
    }])
    .unwrap();
    let visibility = [1_u8; 64 * 4];
    let visibility_buffer = graph
        .buffer(
            "selected geometry",
            BufferDesc {
                size: visibility.len() as u64,
            },
        )
        .unwrap();
    graph
        .upload_buffer("select geometry", visibility_buffer, 0, &visibility)
        .unwrap();
    let args = graph
        .encode_indexed_indirect(
            "encode selected geometry",
            &table,
            draw.vertices,
            draw.indices,
            visibility_buffer,
            0,
        )
        .unwrap();
    let opaque = [Draw::TransformedIndirect {
        args,
        material: 0,
        transform: 0,
    }];
    let materials = [material];
    let transforms = [Transform::IDENTITY];
    let mut frame = Frame::new(&view, &materials, &opaque, &[], &[]);
    frame.transforms = &transforms;
    let scene = recipe.add_scene(&mut graph, frame).unwrap();
    graph
        .readback_image("capture model-space color", scene.hdr)
        .unwrap();
    let graph = graph.compile().unwrap();
    let ordered = graph.ordered_pass_names().collect::<Vec<_>>();
    assert!(ordered.contains(&"encode selected geometry"));
    assert!(ordered.contains(&"pbr transforms upload"));
}

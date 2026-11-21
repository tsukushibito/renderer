//! Shared cold setup and the three public view workflows.
#[path = "toon.rs"]
pub(crate) mod toon;
#[path = "validation.rs"]
pub(crate) mod validation;
use renderer::{
    capacity::Preparation,
    graph::{GraphCapacity, GraphPool, ImageDesc},
    runtime::{Runtime, RuntimeConfig, RuntimeLimits},
};
use renderer_pipeline::{
    material::{AlphaMode, MaterialDefinition, Materials, PbrParams, UnlitParams, Value},
    mesh::{MeshCapacity, MeshData, Meshes},
    passes::{bloom::Bloom, output::SdrOutput},
    pbr::{Bounds, PbrVertex, Projection, SceneLimits, Sun, Technique, Transform, ViewInput},
    pipeline::{
        CameraView, RenderObject, SceneInput, StandardConfig, StandardPipeline, ViewId,
        ViewPipeline, ViewRenderer,
    },
    unlit_forward::{IDENTITY, Vertex},
};
use std::{error::Error, num::NonZeroUsize, time::Duration};

// Each executable selects one of the same shared workflows.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq)]
pub enum Example {
    Textured,
    TwoViews,
    MixedPost,
}

#[allow(dead_code)]
pub fn standard(
    extent: [u32; 2],
    limits: SceneLimits,
    definitions: &[MaterialDefinition],
) -> Result<StandardPipeline, Box<dyn Error>> {
    Ok(StandardPipeline::new(
        Technique::Forward,
        StandardConfig::new(extent),
        limits,
        definitions,
    )?)
}
#[allow(dead_code)]
pub fn run<P: ViewPipeline>(
    example: Example,
    make: impl Fn([u32; 2], SceneLimits, &[MaterialDefinition]) -> Result<P, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let api = match std::env::args().nth(1).as_deref() {
        #[cfg(feature = "vulkan")]
        Some("vulkan") => RuntimeConfig::Vulkan(Default::default()),
        #[cfg(all(windows, feature = "d3d12"))]
        Some("d3d12") => RuntimeConfig::D3D12(Default::default()),
        _ => return Err("pass the enabled GPU backend: vulkan or d3d12".into()),
    };
    run_on(api, example, make)
}
pub fn run_on<P: ViewPipeline>(
    api: RuntimeConfig,
    example: Example,
    make: impl Fn([u32; 2], SceneLimits, &[MaterialDefinition]) -> Result<P, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let mut extent = [64, 64];
    let count = if example == Example::TwoViews { 2 } else { 1 };
    let limits = SceneLimits {
        frames_in_flight: NonZeroUsize::MIN,
        max_draws: 4,
        max_materials: 4,
        max_transforms: 4,
        max_casters_per_cascade: 0,
    };
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 2,
        retired_mesh_versions: 2,
    })?;
    let points = [[-0.6, -0.6, 0.5], [0.6, -0.6, 0.5], [0., 0.6, 0.5]];
    let vertices = |scale| {
        points.map(|p| {
            PbrVertex::new(
                [p[0] * scale, p[1] * scale, p[2]],
                [0., 0., -1.],
                [1., 0., 0., 1.],
                [p[0] + 0.5, p[1] + 0.5],
            )
            .unwrap()
        })
    };
    let geometry = |scale| {
        if example == Example::TwoViews {
            MeshData::unlit(
                &points.map(|p| Vertex {
                    position: [p[0] * scale, p[1] * scale, p[2]],
                    color: [1.; 4],
                }),
                &[0, 2, 1],
            )
        } else {
            MeshData::pbr(&vertices(scale), &[0, 2, 1])
        }
    };
    let model = meshes.create(geometry(1.)?)?;
    let mut materials = Materials::default();
    let mut definitions = Vec::new();
    let mut extra = Vec::new();
    if example == Example::MixedPost {
        let mesh = meshes.create(MeshData::unlit(
            &points.map(|position| Vertex {
                position,
                color: [1.; 4],
            }),
            &[0, 1, 2],
        )?)?;
        let definition = toon::toon(false)?;
        let toon =
            materials.create_custom(&definition, &[("color", Value::Color([0., 1., 0., 1.]))])?;
        let marker = materials.create_unlit(UnlitParams {
            color: [0., 0., 1., 1.],
        })?;
        definitions.push(definition);
        for (material, x) in [(marker, -0.35), (toon, 0.35)] {
            let t = Transform::new([[0.5, 0., 0., x], [0., 0.5, 0., 0.], [0., 0., 1., -0.1]])?;
            extra.push(RenderObject::new(mesh, material).with_transform(t));
        }
    }
    let prepare = |extent| -> Result<_, Box<dyn Error>> {
        let mut budget = Preparation::new(NonZeroUsize::MIN);
        let renderer = ViewRenderer::prepare(
            make(extent, limits, &definitions)?,
            extent,
            NonZeroUsize::new(count).unwrap(),
            Some(SdrOutput::display(false)),
            &mut budget,
        )?;
        Ok((renderer, budget))
    };
    let (mut pipeline, first) = prepare(extent)?;
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    let mut resized = if example != Example::MixedPost {
        let (next, second) = prepare([32, 32])?;
        preparation.add_alternatives(&[&first, &second])?;
        Some(next)
    } else {
        preparation.add_alternatives(&[&first])?;
        None
    };
    meshes.reserve(&mut preparation)?;
    let texture_desc = ImageDesc::rgba8_srgb(2, 2);
    preparation.reserve_image(texture_desc, 1)?;
    let bloom = if example == Example::MixedPost {
        Bloom::Tent
    } else {
        Bloom::Off
    };
    let bloom = bloom.prepare(&mut preparation, ImageDesc::rgba16_float(64, 64))?;
    let capacity = preparation.graph_capacity(GraphCapacity::default())?;
    let mut runtime = Runtime::new(
        api,
        RuntimeLimits {
            graph: capacity,
            ..Default::default()
        },
        preparation.working_set(),
    )?;
    let id = pipeline.initialize(&mut runtime)?;
    let wait = |runtime: &mut Runtime, id| -> Result<(), Box<dyn Error>> {
        drop(
            runtime
                .wait_for_frame(id, Duration::from_secs(15))?
                .ok_or("upload timed out")?,
        );
        Ok(())
    };
    if let Some(id) = id {
        wait(&mut runtime, id)?;
    }
    let mut startup = GraphPool::new(capacity, 1)?;
    let mut graph = startup.begin()?;
    let checker = graph.image("checker", texture_desc)?;
    graph.upload_image(
        "checker",
        checker,
        &[
            255, 255, 255, 255, 80, 80, 80, 255, 80, 80, 80, 255, 255, 255, 255, 255,
        ],
    )?;
    graph.export_image(checker)?;
    let sent = runtime.submit(&graph.compile()?)?;
    let texture = sent.export_image(checker)?;
    wait(&mut runtime, sent.id())?;
    drop(sent);
    let paint = |color| {
        let mut params = PbrParams::color(color);
        params.factors.double_sided = true;
        params.textures.base_color = Some(texture.clone());
        params
    };
    let red = [1., 0.2, 0.1, 1.];
    let material = if example == Example::TwoViews {
        materials.create_unlit(UnlitParams { color: red })?
    } else {
        materials.create_pbr(paint(red))?
    };
    let mut objects = vec![RenderObject::new(model, material)];
    objects.extend(extra);
    if example == Example::MixedPost {
        let mut glass = paint([0.2, 0.5, 1., 0.5]);
        glass.factors.alpha_mode = AlphaMode::Blend;
        objects.push(
            RenderObject::new(model, materials.create_pbr(glass)?).with_transform(Transform::new(
                [[0.6, 0., 0., 0.], [0., 0.6, 0., 0.], [0., 0., 1., -0.2]],
            )?),
        );
    }
    if let Some(id) = meshes.prepare_gpu(&mut runtime, &mut GraphPool::new(capacity, 1)?)? {
        wait(&mut runtime, id)?;
    }
    let mut views = Vec::new();
    for index in 0..count {
        let mut matrix = IDENTITY;
        if index == 1 {
            matrix[0][0] = 0.5;
            matrix[1][1] = 0.5;
        }
        let camera = ViewInput::new(
            matrix,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?;
        let view = CameraView {
            id: ViewId(index as u64),
            extent,
            camera,
            bounds: Bounds::new([-1., -1., 0.], [1., 1., 1.])?,
            sun: Sun::new([0., 0., -1.], [2.; 3])?,
        };
        pipeline.register_view(&view)?;
        views.push(view);
    }
    let mut pool = GraphPool::new(capacity, 1)?;
    let mut previous = [None; 2];
    for step in 0..3 {
        #[cfg(test)]
        if step == 1 {
            let mut stale = pipeline.begin_frame(&mut pool)?;
            assert!(
                stale
                    .draw_view(SceneInput::new(&meshes, &materials, &objects), &views[0])
                    .is_err()
            );
        }
        if step == 1
            && let Some(next) = resized.take()
        {
            extent = [32, 32];
            pipeline.clear(&mut runtime)?;
            pipeline = next;
            if let Some(id) = pipeline.initialize(&mut runtime)? {
                wait(&mut runtime, id)?;
            }
            for camera in &mut views {
                camera.extent = extent;
                pipeline.register_view(camera)?;
            }
            // The old frame has returned, so its mesh registration can be
            // collected before reserving the same buffer class again.
            meshes.collect(&mut runtime)?;
            let id = meshes
                .prepare_gpu(&mut runtime, &mut GraphPool::new(capacity, 1)?)?
                .expect("changed mesh version");
            wait(&mut runtime, id)?;
        }
        if step < 2 {
            if let Some(id) =
                materials.prepare_gpu(&mut runtime, &mut GraphPool::new(capacity, 1)?)?
            {
                wait(&mut runtime, id)?;
            }
            pipeline.prepare_scene(SceneInput::new(&meshes, &materials, &objects))?;
        }
        let scene = SceneInput::new(&meshes, &materials, &objects);
        #[cfg(all(test, feature = "memory-profiling"))]
        let before = renderer::measurement::Snapshot::now();
        let mut frame = pipeline.begin_frame(&mut pool)?;
        let mut reads = [None; 2];
        for (camera, slot) in views.iter().zip(&mut reads) {
            let hdr = frame.draw_view(scene, camera)?.color;
            let hdr = bloom.add_passes(frame.graph(), hdr)?;
            let target = if example == Example::TwoViews {
                Some(
                    frame
                        .graph()
                        .image("camera target", ImageDesc::rgba8(extent[0], extent[1]))?,
                )
            } else {
                None
            };
            let color = frame.write_output(hdr, 1., target)?;
            if let Some(target) = target {
                assert_eq!(target, color.raw_image());
            }
            *slot = Some(
                frame
                    .graph()
                    .readback_image("view pixels", color.raw_image())?,
            );
        }
        if step == 0 {
            // Already recorded operands keep the old versions through submission.
            if resized.is_some() {
                meshes.update(model, geometry(0.75)?)?;
            }
            let green = [0.15, 1., 0.25, 1.];
            if example == Example::TwoViews {
                materials.update_unlit(material, UnlitParams { color: green })?;
            } else {
                materials.update_pbr(material, paint(green))?;
            }
        }
        let sent = frame.submit(&mut runtime)?;
        let mut frame = runtime
            .wait_for_frame(sent.id(), Duration::from_secs(15))?
            .ok_or("frame timed out")?;
        for (i, read) in reads.into_iter().flatten().enumerate() {
            let mut pixels = [0; 64 * 64 * 4];
            let pixels = &mut pixels[..(extent[0] * extent[1] * 4) as usize];
            frame.readback_into(read, pixels)?;
            let center = (extent[0] * (extent[1] / 2) + extent[0] / 2) as usize * 4;
            let color = &pixels[center..center + 4];
            let coverage = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|rgba| rgba[..3] != [0; 3])
                .count();
            if coverage == 0
                || (step == 1 && previous[i] == Some([color[0], color[1], color[2]]))
                || (step == 2 && previous[i] != Some([color[0], color[1], color[2]]))
            {
                return Err("expected visible geometry and a changed material color".into());
            }
            previous[i] = Some([color[0], color[1], color[2]]);
            #[cfg(not(test))]
            println!(
                "frame {step} view {i}: {coverage} lit pixels, center RGBA {:?}",
                color
            );
        }
        drop((frame, sent));
        #[cfg(all(test, feature = "memory-profiling"))]
        if step == 2 {
            let delta = renderer::measurement::Snapshot::now().since(before);
            assert!(delta.is_allocation_free(), "{delta:?}");
        }
        materials.collect_gpu(&mut runtime)?;
    }
    meshes.clear(&mut runtime)?;
    materials.clear(&mut runtime)?;
    pipeline.clear(&mut runtime)?;
    runtime.delete(texture.key())?;
    validation::check_validation(runtime)
}

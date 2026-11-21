//! Shared cold setup and the three public view workflows.
#[path = "toon.rs"]
mod toon;
use renderer::{
    capacity::Preparation,
    graph::{Graph, GraphCapacity, GraphError, GraphPool, ImageDesc},
    runtime::{Runtime, RuntimeConfig, RuntimeLimits},
};
use renderer_pipeline::{
    material::{AlphaMode, Materials, PbrParams, UnlitParams, Value},
    mesh::{MeshCapacity, MeshData, Meshes},
    passes::{
        bloom::{Bloom, PreparedBloom},
        output::SdrImage,
    },
    pbr::{
        Bounds, Config, PbrVertex, Projection, SceneLimits, Sun, Technique, Transform, ViewInput,
    },
    pipeline::{RenderObject, RenderPipeline, SceneInput, View},
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

pub fn run(
    example: Example,
    draw: impl Fn(
        &mut RenderPipeline,
        &mut Graph,
        &mut [View],
        SceneInput<'_>,
        &PreparedBloom,
    ) -> Result<[Option<SdrImage>; 2], GraphError>,
) -> Result<(), Box<dyn Error>> {
    let api = match std::env::args().nth(1).as_deref() {
        #[cfg(feature = "vulkan")]
        Some("vulkan") => RuntimeConfig::Vulkan(Default::default()),
        #[cfg(all(windows, feature = "d3d12"))]
        Some("d3d12") => RuntimeConfig::D3D12(Default::default()),
        _ => return Err("pass the enabled GPU backend: vulkan or d3d12".into()),
    };
    let extent = [64, 64];
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
    let pbr_vertices = points.map(|p| {
        PbrVertex::new(p, [0., 0., -1.], [1., 0., 0., 1.], [p[0] + 0.5, p[1] + 0.5]).unwrap()
    });
    let model = meshes.create(MeshData::pbr(&pbr_vertices, &[0, 2, 1])?)?;
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
    let mut pipeline = RenderPipeline::new(
        Technique::Forward,
        Config::display(extent),
        limits,
        &definitions,
    )?;
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    pipeline.reserve_views(&mut preparation, NonZeroUsize::new(count).unwrap())?;
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
    wait(&mut runtime, id)?;
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
    let material = materials.create_pbr(paint([1., 0.2, 0.1, 1.]))?;
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
        let mut view = View::new(camera, Bounds::new([-1., -1., 0.], [1., 1., 1.])?);
        view.sun = Sun::new([0., 0., -1.], [2.; 3])?;
        views.push(view);
    }
    let mut pool = GraphPool::new(capacity, 1)?;
    let mut previous = [None; 2];
    for step in 0..2 {
        if step == 1 {
            materials.update_pbr(material, paint([0.15, 1., 0.25, 1.]))?;
        }
        if let Some(id) = materials.prepare_gpu(&mut runtime, &mut GraphPool::new(capacity, 1)?)? {
            wait(&mut runtime, id)?;
        }
        let mut graph = pool.begin()?;
        let input = SceneInput::new(&meshes, &materials, &objects);
        let outputs = draw(&mut pipeline, &mut graph, &mut views, input, &bloom)?;
        let mut reads = [None; 2];
        for (output, slot) in outputs.into_iter().zip(&mut reads) {
            if let Some(color) = output {
                *slot = Some(graph.readback_image("view pixels", color.raw_image())?);
            }
        }
        let sent = runtime.submit(&graph.compile()?)?;
        let mut frame = runtime
            .wait_for_frame(sent.id(), Duration::from_secs(15))?
            .ok_or("frame timed out")?;
        for (i, read) in reads.into_iter().flatten().enumerate() {
            let mut pixels = [0; 64 * 64 * 4];
            frame.readback_into(read, &mut pixels)?;
            let center = (extent[0] * (extent[1] / 2) + extent[0] / 2) as usize * 4;
            let color = &pixels[center..center + 4];
            let coverage = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|rgba| rgba[..3] != [0; 3])
                .count();
            if coverage == 0 || previous[i] == Some([color[0], color[1], color[2]]) {
                return Err("expected visible geometry and a changed material color".into());
            }
            previous[i] = Some([color[0], color[1], color[2]]);
            println!(
                "frame {step} view {i}: {coverage} lit pixels, center RGBA {:?}",
                color
            );
        }
        materials.collect_gpu(&mut runtime)?;
    }
    meshes.clear(&mut runtime)?;
    materials.clear(&mut runtime)?;
    pipeline.clear(&mut runtime)?;
    runtime.delete(texture.key())?;
    Ok(())
}

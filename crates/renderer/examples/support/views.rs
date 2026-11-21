//! Shared cold setup and the three public view workflows.
#[path = "toon.rs"]
pub(crate) mod toon;
#[path = "validation.rs"]
pub(crate) mod validation;
use renderer::{
    material::{AlphaMode, MaterialDefinition, Materials, PbrParams, UnlitParams, Value},
    mesh::{MeshCapacity, MeshData, Meshes},
    passes::bloom::Bloom,
    pbr::{Bounds, PbrVertex, Projection, SceneLimits, Sun, Technique, Transform, ViewInput},
    pipeline::{
        CameraGroup, CameraView, PreparedSetup, RenderObject, RendererSetup, SceneAssets,
        SceneInput, StandardConfig, StandardPipeline, ViewId, ViewPipeline,
    },
    unlit_forward::{IDENTITY, Vertex},
};
use renderer_core::{graph::ImageDesc, runtime::RuntimeConfig};
use std::{error::Error, time::Duration};

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
pub fn run<P: ViewPipeline + 'static>(
    example: Example,
    make: impl Fn([u32; 2], SceneLimits, &[MaterialDefinition]) -> Result<P, Box<dyn Error>>
    + Copy
    + 'static,
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
pub fn run_on<P: ViewPipeline + 'static>(
    api: RuntimeConfig,
    example: Example,
    make: impl Fn([u32; 2], SceneLimits, &[MaterialDefinition]) -> Result<P, Box<dyn Error>>
    + Copy
    + 'static,
) -> Result<(), Box<dyn Error>> {
    let mut extent = [64, 64];
    let count = if example == Example::TwoViews { 2 } else { 1 };
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
        for (material, x) in [(marker, -0.35), (toon, 0.35)] {
            let t = Transform::new([[0.5, 0., 0., x], [0., 0.5, 0., 0.], [0., 0., 1., -0.1]])?;
            extra.push(RenderObject::new(mesh, material).with_transform(t));
        }
    }
    let resize = example != Example::MixedPost;
    let texture_desc = ImageDesc::rgba8_srgb(2, 2);
    let red = [1., 0.2, 0.1, 1.];
    let material = if example == Example::TwoViews {
        materials.create_unlit(UnlitParams { color: red })?
    } else {
        materials.create_pbr(PbrParams::color(red))?
    };
    let mut objects = vec![RenderObject::new(model, material)];
    objects.extend(extra);
    let glass_color = [0.2, 0.5, 1., 0.5];
    let glass = if example == Example::MixedPost {
        let handle = materials.create_pbr(PbrParams::color(glass_color))?;
        objects.push(
            RenderObject::new(model, handle).with_transform(Transform::new([
                [0.6, 0., 0., 0.],
                [0., 0.6, 0., 0.],
                [0., 0., 1., -0.2],
            ])?),
        );
        Some(handle)
    } else {
        None
    };
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
        };
        views.push(view);
    }
    let setup = |views: &[CameraView]| -> Result<RendererSetup, Box<dyn Error>> {
        let mut group = CameraGroup::custom(views[0], make);
        for view in &views[1..] {
            group.add_view(*view)?;
        }
        Ok(RendererSetup::new(group))
    };
    let mut plan =
        setup(&views)?.prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
    let prepare_passes =
        |plan: &mut PreparedSetup<'_>, extent: [u32; 2]| -> Result<_, Box<dyn Error>> {
            plan.reserve_texture("checker", texture_desc)?;
            for _ in 0..count {
                plan.reserve_readback("view pixels", ImageDesc::rgba8(extent[0], extent[1]))?;
            }
            let effect = if example == Example::MixedPost {
                Bloom::Tent
            } else {
                Bloom::Off
            };
            Ok(effect.prepare(
                plan.preparation_mut(),
                ImageDesc::rgba16_float(extent[0], extent[1]),
            )?)
        };
    let mut bloom = prepare_passes(&mut plan, extent)?;
    let mut renderer = plan.build(api)?;
    let (upload, texture) = renderer.upload_texture(
        "checker",
        texture_desc,
        &[
            255, 255, 255, 255, 80, 80, 80, 255, 80, 80, 80, 255, 255, 255, 255, 255,
        ],
    )?;
    drop(
        renderer
            .runtime()
            .wait_for_frame(upload, Duration::from_secs(15))?
            .ok_or("texture upload timed out")?,
    );
    let paint = |color| {
        let mut params = PbrParams::color(color);
        params.factors.double_sided = true;
        params.textures.base_color = Some(texture.snapshot().clone());
        params
    };
    if example != Example::TwoViews {
        materials.update_pbr(material, paint(red))?;
        if let Some(handle) = glass {
            let mut params = paint(glass_color);
            params.factors.alpha_mode = AlphaMode::Blend;
            materials.update_pbr(handle, params)?;
        }
        materials.collect_gpu(renderer.runtime())?;
        renderer.prepare_assets(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
    }
    let mut previous = [None; 2];
    for step in 0..3 {
        #[cfg(test)]
        if step == 1 {
            let stale = renderer.begin_frame()?;
            assert!(
                stale
                    .bind_for_view(views[0].id, SceneInput::new(&meshes, &materials, &objects))
                    .is_err()
            );
        }
        if step == 1 && resize {
            extent = [32, 32];
            for camera in &mut views {
                camera.extent = extent;
            }
            let mut plan =
                setup(&views)?.prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
            bloom = prepare_passes(&mut plan, extent)?;
            renderer.replace_working_set(plan)?;
        } else if step == 1 {
            renderer.prepare_assets(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
        }
        let scene = SceneInput::new(&meshes, &materials, &objects)
            .with_sun(Sun::new([0., 0., -1.], [2.; 3])?)
            .with_bounds(Bounds::new([-1., -1., 0.], [1., 1., 1.])?);
        #[cfg(all(test, feature = "memory-profiling"))]
        let before = renderer_core::measurement::Snapshot::now();
        let mut frame = renderer.begin_frame()?;
        let scene = frame.bind_for_view(views[0].id, scene)?;
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
            let color = frame.write_output(camera.id, hdr, 1., target)?;
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
            if resize {
                meshes.update(model, geometry(0.75)?)?;
            }
            let green = [0.15, 1., 0.25, 1.];
            if example == Example::TwoViews {
                materials.update_unlit(material, UnlitParams { color: green })?;
            } else {
                materials.update_pbr(material, paint(green))?;
            }
        }
        let sent = frame.submit()?;
        let mut frame = renderer
            .runtime()
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
            let delta = renderer_core::measurement::Snapshot::now().since(before);
            assert!(delta.is_allocation_free(), "{delta:?}");
        }
        meshes.collect_gpu(renderer.runtime())?;
        materials.collect_gpu(renderer.runtime())?;
    }
    let mut runtime = renderer.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    drop(texture);
    validation::check_validation(runtime)
}

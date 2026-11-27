use sashiko_renderer::{
    material::{
        AlphaMode, MaterialDefinition, MaterialFactors, Materials, Parameter, PbrParams,
        UnlitParams, Value,
    },
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr,
    pipeline::{
        CameraView, RenderObject, SceneInput, StandardConfig, StandardPipeline, ViewId,
        ViewRenderer,
    },
    unlit_forward::{self, IDENTITY},
};
use sashiko_renderer_core::{
    capacity::Preparation,
    graph::{BufferDesc, GraphCapacity, GraphPool, SampleCount},
    program::ManagedRasterProgram,
    runtime::{Runtime, RuntimeConfig, RuntimeLimits},
};
use std::{error::Error, num::NonZeroUsize, time::Duration};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[path = "../../examples/support/toon.rs"]
mod toon;
use toon::toon;
fn quad(pbr: bool, baked: Option<f32>) -> Result<MeshData> {
    let points = [[-1., -1., 0.], [1., -1., 0.], [1., 1., 0.], [-1., 1., 0.]]
        .map(|[x, y, z]| baked.map_or([x, y, z], |depth| [x * 0.25, y, depth]));
    let indices = [0, 1, 2, 2, 3, 0];
    if pbr {
        let vertices = points
            .map(|p| pbr::PbrVertex::new(p, [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap());
        Ok(MeshData::pbr(&vertices, &indices)?)
    } else {
        Ok(MeshData::unlit(
            &points.map(|position| unlit_forward::Vertex {
                position,
                color: [1.; 4],
            }),
            &indices,
        )?)
    }
}
fn check(config: RuntimeConfig, technique: pbr::Technique) -> Result<()> {
    let opaque = toon(false)?;
    let blend = toon(true)?;
    let slot = |name: &str, group, binding, default| Parameter::Binding {
        name: name.into(),
        group,
        binding,
        default,
    };
    let textured = MaterialDefinition::new(
        ManagedRasterProgram::textured_mesh_hdr(),
        None,
        AlphaMode::Opaque,
        vec![
            Parameter::Constant {
                name: "color".into(),
                word: 16,
                default: Value::Color([1.; 4]),
            },
            slot("offset", 0, 0, None),
            slot("albedo", 1, 0, None),
            slot(
                "filter",
                1,
                1,
                Some(Value::Sampler(
                    sashiko_renderer_core::program::SamplerDescriptor::NearestClamp,
                )),
            ),
        ],
    )?;
    let mut materials = Materials::default();
    let brick = |alpha_mode, alpha| PbrParams {
        factors: MaterialFactors {
            base_color: [0., 0., 0., alpha],
            emissive_nits: [1., 0., 0.],
            double_sided: true,
            alpha_mode,
            ..Default::default()
        },
        ..Default::default()
    };
    let floor = materials.create_pbr(brick(AlphaMode::Opaque, 1.))?;
    let glass = materials.create_pbr(brick(AlphaMode::Blend, 0.5))?;
    let unlit = materials.create_unlit(UnlitParams {
        color: [0., 0., 1., 1.],
    })?;
    let green = materials.create_custom(&opaque, &[("color", Value::Color([0., 0.6, 0., 1.]))])?;
    let blue = materials.create_custom(&blend, &[("color", Value::Color([0., 0., 1., 0.5]))])?;
    let mut standard = StandardPipeline::new(
        technique,
        StandardConfig {
            film_output: false,
            extent: [32, 16],
            clear_color: [0.; 4],
            shadows: (technique == pbr::Technique::Deferred).then_some(pbr::ShadowSettings {
                resolution: 32,
                ..Default::default()
            }),
            samples: SampleCount::X1,
        },
        pbr::SceneLimits {
            max_indirect_shadow_capsules: 0,
            max_lightmapped_draws: 0,
            frames_in_flight: NonZeroUsize::MIN,
            max_draws: 8,
            max_materials: 6,
            max_shadowed_rect_lights: 2,
            max_local_lights: 2,
            max_shadow_casters: 1,
            max_transforms: 1,
        },
        &[opaque, blend, textured.clone()],
    )?;
    // Deferred also exercises the shadow histories with parallel object classification.
    if technique == pbr::Technique::Deferred {
        standard.set_scene_jobs(
            sashiko_jobs::JobSystem::new(4.try_into().unwrap())?.into(),
            NonZeroUsize::MIN,
        );
    }
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 4,
        retired_mesh_versions: 4,
    })?;
    let pbr_mesh = meshes.create(quad(true, None)?)?;
    let mesh = meshes.create(quad(false, None)?)?;
    let glass_mesh = meshes.create(quad(true, Some(0.3))?)?;
    let blue_mesh = meshes.create(quad(false, Some(0.2))?)?;
    let mut prep = Preparation::new(NonZeroUsize::MIN);
    let mut scene = ViewRenderer::new();
    let group = scene.prepare_pipeline(standard, [32, 16], NonZeroUsize::MIN, None, &mut prep)?;
    meshes.reserve(&mut prep)?;
    let offset_desc = BufferDesc { size: 16 };
    let map_desc = sashiko_renderer_core::graph::ImageDesc::rgba8(1, 1);
    prep.reserve_buffer(offset_desc, 1)?;
    prep.reserve_image(map_desc, 2)?;
    let capacity = prep.graph_capacity(GraphCapacity::default())?;
    let mut pool = GraphPool::new(capacity, 1)?;
    let mut runtime = Runtime::new(
        config,
        RuntimeLimits {
            graph: capacity,
            ..Default::default()
        },
        prep.working_set(),
    )?;
    let id = scene.initialize_pipeline(group, &mut runtime)?.unwrap();
    drop(
        runtime
            .wait_for_frame(id, Duration::from_secs(10))?
            .ok_or("default upload timed out")?,
    );
    // The resident uniform retains the startup upload's provenance. Keep its
    // graph storage separate from the slot recycled by normal frames.
    let mut startup = GraphPool::new(capacity, 1)?;
    let mut graph = startup.begin()?;
    let offset = graph.buffer("vertex offset", offset_desc)?;
    graph.upload_buffer("zero offset", offset, 0, &[0; 16])?;
    graph.export_buffer(offset)?;
    let mut map = |pixel: &[u8]| -> Result<_> {
        let image = graph.image("swatch", map_desc)?;
        graph.upload_image("color", image, pixel)?;
        graph.export_image(image)?;
        Ok(image)
    };
    let cyan = map(&[0, 255, 255, 255])?;
    let yellow = map(&[255, 255, 0, 255])?;
    let plan = graph.compile()?;
    let pending = runtime.submit(&plan)?;
    let offset = pending.export_buffer(offset)?;
    let cyan = pending.export_image(cyan)?;
    let yellow = pending.export_image(yellow)?;
    drop(
        runtime
            .wait_for_frame(pending.id(), Duration::from_secs(10))?
            .ok_or("texture upload timed out")?,
    );
    drop((pending, plan));
    let mut masked = brick(AlphaMode::Mask, 1.);
    masked.textures.base_color = Some(cyan.clone().into());
    materials.update_pbr(floor, masked)?;
    let swatch = materials.create_custom(
        &textured,
        &[
            ("offset", Value::Uniform(offset.clone())),
            ("albedo", Value::Texture(cyan.clone())),
        ],
    )?;
    materials.prepare_samplers(&mut runtime, [floor, glass, unlit, green, blue, swatch])?;
    let mut bytes = [0u8; 32 * 16 * 8];
    let input = pbr::ViewInput::new(
        IDENTITY,
        [0., 0., -1.],
        [0., 0., 1.],
        [1., 2.],
        pbr::Projection::Orthographic,
    )?;
    if let Some(id) = meshes.prepare_gpu(&mut runtime, &mut GraphPool::new(capacity, 1)?)? {
        drop(
            runtime
                .wait_for_frame(id, Duration::from_secs(10))?
                .ok_or("mesh upload timeout")?,
        );
    }
    if let Some(id) = materials.prepare_gpu(&mut runtime, &mut GraphPool::new(capacity, 1)?)? {
        drop(
            runtime
                .wait_for_frame(id, Duration::from_secs(10))?
                .ok_or("material upload timeout")?,
        );
    }
    let view = CameraView {
        id: ViewId(1),
        extent: [32, 16],
        camera: input,
    };
    scene.register_view(group, &view)?;
    let shadow_light = [pbr::LocalLight::shadowed_rect(pbr::RectLight::from_lumens(
        [0., 2., 0.],
        [1., 0., 0.],
        [0., 0., 1.],
        [1.; 3],
        0.,
        5.,
    )?)];
    for index in 0..4 {
        #[cfg(feature = "memory-profiling")]
        let before = sashiko_renderer_core::measurement::Snapshot::now();
        let draw = |mesh, mat, width, x, z| -> Result<RenderObject> {
            let transform =
                pbr::Transform::new([[width, 0., 0., x], [0., 1., 0., 0.], [0., 0., 1., z]])?;
            Ok(RenderObject {
                transform,
                casts_shadow: mat == green,
                ..RenderObject::new(mesh, mat)
            })
        };
        let mut draws = [
            draw(blue_mesh, blue, 1., 0., 0.)?,
            draw(mesh, unlit, 1., 0., 0.8)?,
            draw(mesh, green, 0.55, 0., 0.4)?,
            draw(glass_mesh, glass, 1., 0., 0.)?,
            draw(pbr_mesh, floor, 1., 0., 0.5)?,
            // Multiple primitives of one model share the single reserved transform.
            draw(pbr_mesh, floor, 1., 0., 0.5)?,
            draw(mesh, unlit, 0.1, -0.7, 0.2)?,
            draw(mesh, swatch, 0.1, 0.8, 0.2)?,
        ];
        if index % 2 == 1 {
            draws.swap(0, 3);
        }
        let input = SceneInput::new(&meshes, &materials, &draws)
            .with_lights(&shadow_light)
            .with_bounds(pbr::Bounds::new([-1., -1., 0.], [1., 1., 1.])?)
            .with_sun(pbr::Sun::from_lux([0., 0., -1.], [1.; 3], 0.)?);
        if index <= 1 {
            scene.prepare_scene(group, input)?;
        }
        let mut recording = scene.begin_frame(&mut pool)?;
        let input = recording.bind_scene(group, input)?;
        let mut before_forward = None;
        if technique == pbr::Technique::Deferred {
            recording.draw_view_with_gbuffer(input, &view, |g, b| {
                before_forward = Some(g.readback_image("PBR depth", b.depth)?);
                Ok(())
            })?
        } else {
            recording.draw_view(input, &view)?
        };
        let output = recording.standard_output(view.id).unwrap();
        assert_eq!(output.reused_rectangle_shadows(), Some(index >= 2));
        if technique == pbr::Technique::Deferred && index >= 2 {
            assert_eq!(output.reused_shadows(), Some([true; 4]));
        }
        let images = output.scene;
        let graph = recording.graph();
        assert!(images.gbuffer.is_none());
        let final_depth = if before_forward.is_some() {
            Some(graph.readback_image("final depth", images.depth.unwrap())?)
        } else {
            None
        };
        let read = graph.readback_image("mixed hdr", images.hdr)?;
        let shadows = images
            .shadows
            .map(|maps| graph.readback_image("mixed caster depth", maps[3]))
            .transpose()?;
        let pending = recording.submit(&mut runtime)?;
        if index == 0 {
            materials.update_custom(swatch, &[("albedo", Value::Texture(yellow.clone()))])?;
            materials.update_custom(green, &[("color", Value::Color([0., 0.3, 0., 1.]))])?;
        }
        let mut frame = runtime
            .wait_for_frame(pending.id(), Duration::from_secs(10))?
            .ok_or("mixed frame timed out")?;
        frame.readback_into(read, &mut bytes)?;
        if let (Some(a), Some(b)) = (before_forward, final_depth) {
            let mut raw = [0u8; 32 * 16 * 4];
            for (read, expected) in [(a, 0.5), (b, 0.4)] {
                frame.readback_into(read, &mut raw)?;
                let start = (32 * 8 + 9) * 4;
                assert!(
                    (f32::from_le_bytes(raw[start..start + 4].try_into()?) - expected).abs()
                        < 0.0001
                );
            }
        }
        if let Some(read) = shadows {
            let mut depth = [0; 32 * 32 * 4];
            frame.readback_into(read, &mut depth)?;
            assert!(
                depth
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| f32::from_le_bytes(*pixel) < 1.0),
                "custom caster wrote no depth"
            );
        }
        drop((frame, pending));
        #[cfg(feature = "memory-profiling")]
        if index >= 2 {
            let delta = sashiko_renderer_core::measurement::Snapshot::now().since(before);
            assert!(delta.is_allocation_free(), "{delta:?}");
        }
        let pixel = |x: usize| -> [f32; 3] {
            let start = (32 * 8 + x) * 8;
            std::array::from_fn(|i| {
                let j = start + i * 2;
                let bits = u16::from_le_bytes([bytes[j], bytes[j + 1]]);
                if bits == 0 {
                    0.
                } else {
                    (1. + f32::from(bits & 1023) / 1024.) * 2f32.powi(i32::from(bits >> 10) - 15)
                }
            })
        };
        let green = if index == 0 { 2. / 3. } else { 1. / 3. };
        for (x, expected) in [
            (1, [1., 0., 0.]),
            (
                28,
                if index == 0 {
                    [0., 1., 1.]
                } else {
                    [1., 1., 0.]
                },
            ),
            (4, [0., 0., 1.]),
            (9, [0., green, 0.]),
            (16, [0.25, green * 0.25, 0.5]),
        ] {
            let actual = pixel(x);
            assert!(
                actual
                    .iter()
                    .zip(expected)
                    .all(|(a, e)| (a - e).abs() < 0.012),
                "{technique:?} frame {index} x={x}: {actual:?} expected {expected:?}"
            );
        }
    }
    // A single textured caster under the first panel, well outside the second panel's range.
    let panel = |x, flux| -> Result<_> {
        Ok(pbr::LocalLight::shadowed_rect(pbr::RectLight::from_lumens(
            [x, 3., 0.],
            [0.5, 0., 0.],
            [0., 0., 0.5],
            [1.; 3],
            flux,
            6.,
        )?))
    };
    let near = panel(0., 10.)?;
    let far = panel(40., 20.)?;
    let replacement = panel(-40., 20.)?;
    let oriented = [near, far];
    let reordered = [far, panel(0., 5.)?];
    let replaced = [replacement, near];
    for step in 0..13 {
        let shift = if matches!(step, 3 | 5..=12) { 1.0 } else { 0.0 };
        let objects = [
            RenderObject::new(pbr_mesh, floor).with_transform(pbr::Transform::new([
                [0.2, 0., 0., shift],
                [0., 0.2, 0., 0.],
                [0., 0., 0.2, 0.2],
            ])?),
        ];
        let lights = match step {
            0 | 1 => &oriented[..],
            6 => &oriented[..1],
            7..=12 => &replaced[..],
            _ => &reordered[..],
        };
        let input = SceneInput::new(&meshes, &materials, &objects)
            .with_lights(lights)
            .with_bounds(pbr::Bounds::new([-2.; 3], [2.; 3])?);
        scene.prepare_scene(group, input)?;
        let mut frame = scene.begin_frame(&mut pool)?;
        let modified = if matches!(step, 8 | 10) {
            let g = frame.graph();
            let image = g.import_image("modified alpha coverage", &cyan)?;
            let alpha = if step == 8 { 0 } else { 192 };
            g.upload_image("edit coverage", image, &[0, 255, 255, alpha])?;
            g.export_image(image)?;
            Some(image)
        } else {
            None
        };
        frame.draw_scene(&view, input)?;
        let faces = frame
            .standard_output(view.id)
            .unwrap()
            .rendered_rectangle_faces();
        match step {
            0 => assert_eq!(faces, 10),
            1 | 2 | 4 | 6 | 9 | 12 => assert_eq!(faces, 0, "step {step}"),
            7 => assert_eq!(faces, 5),
            _ => assert_eq!(faces, 1, "step {step}"),
        }
        // Abandon the first pose edit and the first alpha upload.
        if step == 3 || step == 8 {
            continue;
        }
        let submission = frame.submit(&mut runtime)?;
        if let Some(image) = modified {
            let updated = submission.export_image(image)?;
            assert_ne!(updated.revision(), cyan.revision());
            let mut masked = brick(AlphaMode::Mask, 1.);
            masked.textures.base_color = Some(updated.into());
            materials.update_pbr(floor, masked)?;
        }
        drop(
            runtime
                .wait_for_frame(submission.id(), Duration::from_secs(10))?
                .ok_or("shadow frame timed out")?,
        );
        if modified.is_some()
            && let Some(id) =
                materials.prepare_gpu(&mut runtime, &mut GraphPool::new(capacity, 1)?)?
        {
            drop(
                runtime
                    .wait_for_frame(id, Duration::from_secs(10))?
                    .ok_or("mask update timed out")?,
            );
        }
    }
    if technique == pbr::Technique::Forward {
        let floor_quad = RenderObject::new(pbr_mesh, floor).with_transform(pbr::Transform::new([
            [0.5, 0., 0., 0.],
            [0., 0.5, 0., 0.],
            [0., 0., 1., 0.5],
        ])?);
        let floor_rgb = &bytes[(32 * 8 + 1) * 8..][..6];
        draw_after_resize(
            &mut scene,
            group,
            &mut runtime,
            &mut pool,
            SceneInput::new(&meshes, &materials, &[floor_quad]),
            view,
            floor_rgb,
        )?;
    }
    scene.clear(&mut runtime)?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    for key in [offset.key(), cyan.key(), yellow.key()] {
        runtime.delete(key)?;
    }
    super::runtime::check_validation(runtime)
}

/// The new extent is absent from the initial working set. Reuse the mesh and
/// resident material registrations without refreshing or uploading either.
fn draw_after_resize(
    scene: &mut ViewRenderer,
    old_group: sashiko_renderer::pipeline::PipelineId,
    runtime: &mut Runtime,
    pool: &mut GraphPool,
    input: SceneInput<'_>,
    previous_view: CameraView,
    floor_rgb: &[u8],
) -> Result<()> {
    let view = CameraView {
        extent: [23, 13],
        ..previous_view
    };
    let n = NonZeroUsize::MIN;
    let limits = pbr::SceneLimits {
        max_indirect_shadow_capsules: 0,
        max_lightmapped_draws: 0,
        frames_in_flight: n,
        max_draws: 1,
        max_materials: 1,
        max_shadowed_rect_lights: 0,
        max_local_lights: 0,
        max_shadow_casters: 0,
        max_transforms: 1,
    };
    let mut prep = Preparation::new(n);
    let next = scene.prepare_pipeline(
        StandardPipeline::forward(view.extent, limits)?,
        view.extent,
        n,
        None,
        &mut prep,
    )?;
    runtime.prepare_working_set(prep.working_set())?;
    input
        .materials
        .prepare_samplers(runtime, input.objects.iter().map(|object| object.material))?;
    scene.reconfigure_view(next, &view, runtime)?;
    scene.remove_pipeline(old_group, runtime)?;
    if let Some(id) = scene.initialize_pipeline(next, runtime)? {
        runtime
            .wait_for_frame(id, Duration::from_secs(10))?
            .ok_or("resize setup timed out")?;
    }
    scene.prepare_scene(next, input)?;
    let mut frame = scene.begin_frame(pool)?;
    let hdr = frame
        .draw_view(frame.bind_scene(next, input)?, &view)?
        .color;
    let read = frame.graph().readback_image("resized HDR", hdr)?;
    let pending = frame.submit(runtime)?;
    let mut completed = runtime
        .wait_for_frame(pending.id(), Duration::from_secs(10))?
        .ok_or("resized frame timed out")?;
    let mut pixels = [0; 23 * 13 * 8];
    completed.readback_into(read, &mut pixels)?;
    assert_eq!(&pixels[(23 * 6 + 11) * 8..][..6], floor_rgb);
    assert_eq!(&pixels[..6], &[0; 6]);
    drop((completed, pending));
    Ok(())
}
#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires real Vulkan GPU"]
fn vulkan_mixed_materials() -> Result<()> {
    for t in [pbr::Technique::Forward, pbr::Technique::Deferred] {
        check(RuntimeConfig::Vulkan(Default::default()), t)?
    }
    Ok(())
}
#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires real D3D12 GPU"]
fn d3d12_mixed_materials() -> Result<()> {
    for t in [pbr::Technique::Forward, pbr::Technique::Deferred] {
        check(RuntimeConfig::D3D12(Default::default()), t)?
    }
    Ok(())
}

/// A full material set shares one upload submission. Updating one entry must
/// fit a Graph with space for exactly one factors buffer and upload pass.
fn residency(config: RuntimeConfig) -> Result<()> {
    let mut materials = Materials::default();
    let mut handles = Vec::new();
    for i in 0..100 {
        handles.push(materials.create_pbr(PbrParams::color([i as f32 / 100., 0., 0., 1.]))?);
    }
    let desc = pbr::MaterialUniform::DESC;
    let mut prep = Preparation::new(NonZeroUsize::MIN);
    prep.reserve_buffer(desc, 101)?;
    let mut runtime = Runtime::new(
        config,
        RuntimeLimits {
            resource_capacity: 101,
            frames: 1,
            results: 1,
            ..Default::default()
        },
        prep.working_set(),
    )?;
    let mut pool = GraphPool::new(GraphCapacity::default(), 1)?;
    let id = materials
        .prepare_gpu(&mut runtime, &mut pool)?
        .expect("initial upload");
    drop(
        runtime
            .wait_for_frame(id, Duration::from_secs(15))?
            .ok_or("upload timed out")?,
    );
    assert!(materials.prepare_gpu(&mut runtime, &mut pool)?.is_none());
    let mut single = GraphPool::new(
        GraphCapacity {
            resources: 1,
            passes: 1,
            upload_bytes: desc.size as usize,
            ..Default::default()
        },
        1,
    )?;
    let keep = materials.snapshot(handles[25])?;
    materials.update_pbr(handles[25], PbrParams::color([0., 1., 0., 1.]))?;
    let id = materials
        .prepare_gpu(&mut runtime, &mut single)?
        .expect("changed factors upload");
    materials.collect_gpu(&mut runtime)?;
    drop(
        runtime
            .wait_for_frame(id, Duration::from_secs(15))?
            .ok_or("update timed out")?,
    );
    assert!(materials.prepare_gpu(&mut runtime, &mut single)?.is_none());
    drop(keep);
    materials.collect_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    super::runtime::check_validation(runtime)
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires real Vulkan GPU"]
fn vulkan_material_residency() -> Result<()> {
    residency(RuntimeConfig::Vulkan(
        sashiko_renderer_core::runtime::VulkanConfig {
            staging_ranges: 128,
            ..Default::default()
        },
    ))
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires real D3D12 GPU"]
fn d3d12_material_residency() -> Result<()> {
    residency(RuntimeConfig::D3D12(
        sashiko_renderer_core::runtime::D3D12Config {
            buffer_count: 128,
            ..Default::default()
        },
    ))
}

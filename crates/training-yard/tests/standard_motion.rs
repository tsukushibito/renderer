#![cfg(all(
    feature = "graphics",
    any(feature = "vulkan", all(windows, feature = "d3d12"))
))]
use renderer::Renderer;
use renderer::scene::Scene;
use renderer::{
    material::{Materials, PbrParams, Value},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{PbrVertex, Projection, Transform, ViewInput},
    pipeline::{CameraView, PipelineResult, RenderObject, SceneAssets, SceneInput, ViewId},
    unlit_forward::{IDENTITY, Vertex},
};
use renderer_core::runtime::RuntimeConfig;
use std::time::Duration;

#[path = "../../renderer/examples/support/validation.rs"]
mod validation;

mod shader {
    include!("../../renderer/examples/support/toon.rs");
}

fn exercise(backend: RuntimeConfig, load_after_start: bool) -> PipelineResult<()> {
    let camera = CameraView {
        id: ViewId(1),
        extent: [32, 16],
        camera: ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
    };
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 2,
        retired_mesh_versions: 2,
    })?;
    let positions = [[-0.9, -0.6, 0.5], [-0.5, 0.6, 0.5], [-0.1, -0.6, 0.5]];
    let pbr = meshes.create(MeshData::pbr(
        &positions.map(|p| PbrVertex::new(p, [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap()),
        &[0, 1, 2],
    )?)?;
    let toon = meshes.create(MeshData::unlit(
        &positions.map(|[x, y, z]| Vertex {
            position: [x + 1., y, z],
            color: [1.; 4],
        }),
        &[0, 1, 2],
    )?)?;
    let mut materials = Materials::default();
    let mut params = PbrParams::color([1.; 4]);
    params.factors.emissive_nits = [1., 0., 0.];
    params.factors.double_sided = true;
    let red = materials.create_pbr(params)?;
    let green = materials.create_custom(
        &shader::toon(false)?,
        &[("color", Value::Color([0., 1., 0., 1.]))],
    )?;
    let mut scene = Scene::default();
    if !load_after_start {
        scene.spawn(RenderObject::new(toon, green));
    }
    let mut view = Renderer::headless(
        backend,
        camera,
        SceneAssets::new(&mut meshes, &mut materials, scene.objects()),
    )?;
    // A UI-only frame uses the same target, without drawing the prepared scene.
    let mut ui = view.begin_frame()?;
    ui.draw_overlay(&[renderer::canvas::Layer {
        paint: renderer::canvas::Paint::Color([0.25, 0., 0., 0.5]),
        rect: renderer::core::graph::RasterRect::new(0, 0, 2, 1),
        clip: Some(renderer::core::graph::RasterRect::new(1, 0, 1, 1)),
    }])?;
    let ticket = ui.capture()?;
    let mut ui_pixels = [0; 32 * 16 * 4];
    assert!(view.read_capture(&ticket, &mut ui_pixels, Duration::from_secs(15))?);
    assert_eq!(&ui_pixels[..8], &[0, 0, 0, 255, 137, 0, 0, 255]);
    let actor = scene.spawn(RenderObject::new(pbr, red));
    view.prepare_scene(SceneAssets::new(
        &mut meshes,
        &mut materials,
        scene.objects(),
    ))?;
    let mut capture = |objects: &[RenderObject]| -> PipelineResult<_> {
        let mut frame = view.begin_frame()?;
        frame.draw_scene(SceneInput::new(&meshes, &materials, objects), 1.)?;
        frame.draw_overlay(&[renderer::canvas::Layer {
            paint: renderer::canvas::Paint::Color([0., 0., 0.25, 1.]),
            rect: renderer::core::graph::RasterRect::new(31, 15, 1, 1),
            clip: None,
        }])?;
        let pending = frame.capture()?;
        let mut pixels = [0; 32 * 16 * 4];
        assert!(
            view.read_capture(&pending, &mut [], Duration::ZERO)
                .is_err()
        );
        assert!(view.read_capture(&pending, &mut pixels, Duration::from_secs(15))?);
        assert_eq!(&pixels[pixels.len() - 4..], &[0, 0, 137, 255]);
        Ok(pixels)
    };
    let original = capture(scene.objects())?;
    let motion = Transform::new([[1., 0., 0., 0.25], [0., 1., 0., 0.], [0., 0., 1., 0.]])?;
    scene.instance_mut(actor).unwrap().transform = motion;
    let moved = capture(scene.objects())?;
    let mut host = Vec::new();
    if !load_after_start {
        host.push(RenderObject::new(toon, green));
    }
    host.push(RenderObject::new(pbr, red).with_transform(motion));
    assert_eq!(moved, capture(&host)?);
    let red_pixels = |pixels: &[u8]| {
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > p[1] && p[0] > p[2])
            .count()
    };
    assert!(red_pixels(&original) > 0);
    assert_eq!(red_pixels(&original), red_pixels(&moved));
    assert_ne!(original, moved);
    assert_eq!(
        moved
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[1] > p[0] && p[1] > p[2]),
        !load_after_start
    );
    scene.remove(actor).unwrap();
    assert_eq!(red_pixels(&capture(scene.objects())?), 0);
    if load_after_start {
        for i in 0..17 {
            let pose = Transform::new([
                [1., 0., 0., i as f32 / 128.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
            ])?;
            scene.spawn(RenderObject::new(pbr, red).with_transform(pose));
        }
        scene.spawn(RenderObject::new(toon, green));
        view.prepare_scene(SceneAssets::new(
            &mut meshes,
            &mut materials,
            scene.objects(),
        ))?;
        let mut frame = view.begin_frame()?;
        frame.draw_scene(SceneInput::new(&meshes, &materials, scene.objects()), 1.)?;
        let loaded = frame.capture()?;
        let mut pixels = [0; 32 * 16 * 4];
        assert!(view.read_capture(&loaded, &mut pixels, Duration::from_secs(15))?);
        assert!(red_pixels(&pixels) > 0);
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[1] > p[0] && p[1] > p[2])
        );
    }
    // Enable a light after startup, then promote it to a shadow caster.
    let panel = renderer::pbr::RectLight::from_lumens(
        [0., 2., 0.],
        [0.5, 0., 0.],
        [0., 0., 0.5],
        [1.; 3],
        15.,
        4.,
    )?;
    let lit = [renderer::pbr::LocalLight::Rect(panel)];
    let shadowed = [renderer::pbr::LocalLight::shadowed_rect(panel)];
    for (lights, kind) in [
        (&lit, renderer::pbr::SceneLimits::LOCAL_LIGHTS),
        (&shadowed, renderer::pbr::SceneLimits::SHADOWED_RECT_LIGHTS),
    ] {
        {
            let mut frame = view.begin_frame()?;
            let input = SceneInput::new(&meshes, &materials, scene.objects());
            let error = frame.draw_scene(input.with_lights(lights), 1.).unwrap_err();
            let room = error.capacity().unwrap();
            assert_eq!((room.kind, room.limit, room.required), (kind, 0, Some(1)));
            frame.draw_scene(input, 1.)?;
        }
        view.prepare_scene(
            SceneAssets::new(&mut meshes, &mut materials, scene.objects()).with_lights(lights),
        )?;
    }
    for lights in [&shadowed, &lit, &shadowed] {
        let mut frame = view.begin_frame()?;
        frame.draw_scene(
            SceneInput::new(&meshes, &materials, scene.objects()).with_lights(lights),
            1.,
        )?;
        let image = frame.capture()?;
        let mut pixels = [0; 32 * 16 * 4];
        assert!(view.read_capture(&image, &mut pixels, Duration::from_secs(15))?);
    }
    let mut runtime = view.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    validation::check_validation(runtime)
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan device"]
fn vulkan_standard_motion() -> PipelineResult<()> {
    for empty in [true, false] {
        exercise(RuntimeConfig::Vulkan(Default::default()), empty)?;
    }
    Ok(())
}
#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a D3D12 device"]
fn d3d12_standard_motion() -> PipelineResult<()> {
    for empty in [true, false] {
        exercise(RuntimeConfig::D3D12(Default::default()), empty)?;
    }
    Ok(())
}

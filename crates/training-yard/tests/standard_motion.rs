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

mod shader {
    include!("../../renderer/examples/support/toon.rs");
}

fn exercise(backend: RuntimeConfig) -> PipelineResult<()> {
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
    params.factors.emissive = [1., 0., 0.];
    params.factors.double_sided = true;
    let red = materials.create_pbr(params)?;
    let green = materials.create_custom(
        &shader::toon(false)?,
        &[("color", Value::Color([0., 1., 0., 1.]))],
    )?;
    let mut scene = Scene::default();
    let actor = scene.spawn(RenderObject::new(pbr, red));
    scene.spawn(RenderObject::new(toon, green));
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
    let mut capture = |objects: &[RenderObject]| -> PipelineResult<_> {
        let mut frame = view.begin_frame()?;
        frame.draw_scene(SceneInput::new(&meshes, &materials, objects), 1.)?;
        let pending = frame.capture()?;
        let mut pixels = [0; 32 * 16 * 4];
        assert!(
            view.read_capture(&pending, &mut [], Duration::ZERO)
                .is_err()
        );
        assert!(view.read_capture(&pending, &mut pixels, Duration::from_secs(15))?);
        Ok(pixels)
    };
    let original = capture(scene.objects())?;
    let motion = Transform::new([[1., 0., 0., 0.25], [0., 1., 0., 0.], [0., 0., 1., 0.]])?;
    scene.instance_mut(actor).unwrap().transform = motion;
    let moved = capture(scene.objects())?;
    let host = [
        RenderObject::new(pbr, red).with_transform(motion),
        RenderObject::new(toon, green),
    ];
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
    assert!(
        moved
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[1] > p[0] && p[1] > p[2])
    );
    let mut runtime = view.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    Ok(())
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan device"]
fn vulkan_standard_motion() -> PipelineResult<()> {
    exercise(RuntimeConfig::Vulkan(Default::default()))
}
#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a D3D12 device"]
fn d3d12_standard_motion() -> PipelineResult<()> {
    exercise(RuntimeConfig::D3D12(Default::default()))
}

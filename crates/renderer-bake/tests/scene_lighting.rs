#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::{
    Renderer,
    core::runtime::RuntimeConfig,
    material::{Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{PbrVertex, Projection, ViewInput},
    pipeline::*,
    unlit_forward::IDENTITY,
};
use renderer_bake::{BakeBackend, BakeScene, Lighting, ProbeSettings, ReflectionProbeDesc};
use std::time::Duration;

mod validation {
    use renderer::core as renderer_core;
    include!("../../renderer/examples/support/validation.rs");
}

fn draw(backend: RuntimeConfig) -> PipelineResult<()> {
    let scene = BakeScene::new(vec![], vec![], vec![])?;
    let light = Lighting::uniform_sky([1., 0., 0.]);
    let diffuse = scene.bake_probes(
        light,
        ProbeSettings {
            backend: BakeBackend::Cpu,
            ..Default::default()
        },
    )?;
    let reflections = scene.bake_reflection_probes(
        &[ReflectionProbeDesc {
            position: [0.; 3],
            min: [-2.; 3],
            max: [2.; 3],
            blend_distance: 0.5,
        }],
        light,
    )?;
    let mut file = Vec::new();
    diffuse.write(&mut file)?;
    let diffuse = renderer::pbr::BakedLighting::from_bytes(&file)?;
    assert_eq!(diffuse.scenario_count(), 1);
    file.clear();
    reflections.write(&mut file)?;
    let reflections = renderer::pbr::ReflectionProbes::from_bytes(&file)?;
    assert_eq!(reflections.extent(), [128, 128]);
    assert_eq!(reflections.scenario_count(), 1);
    let view = CameraView {
        id: ViewId(0),
        extent: [32, 32],
        camera: ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
    };
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let vertices = [[-0.8, -0.8, 0.5], [0.8, -0.8, 0.5], [0., 0.8, 0.5]]
        .map(|p| PbrVertex::new(p, [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap());
    let mesh = meshes.create(MeshData::pbr(&vertices, &[0, 2, 1])?.with_lightmap_uvs(&[
        [0., 0.],
        [1., 0.],
        [0., 1.],
    ])?)?;
    let mut materials = Materials::default();
    let mut metal = PbrParams::color([1.; 4]);
    metal.factors.double_sided = true;
    metal.factors.metallic = 1.;
    let mut matte = PbrParams::color([1.; 4]);
    matte.factors.double_sided = true;
    let matte = materials.create_pbr(matte)?;
    let translated =
        renderer::pbr::Transform::new([[1., 0., 0., 0.03], [0., 1., 0., 0.], [0., 0., 1., 0.]])?;
    let objects = [
        RenderObject::new(mesh, materials.create_pbr(metal)?),
        RenderObject::new(mesh, matte),
        RenderObject::new(mesh, matte).with_lightmap(40),
        RenderObject::new(mesh, matte)
            .with_transform(translated)
            .with_lightmap(41),
    ];
    let mut renderer = Renderer::headless(
        backend,
        view,
        SceneAssets::new(&mut meshes, &mut materials, &objects),
    )?;
    for (include_diffuse, include_reflections) in [(false, true), (true, false), (true, true)] {
        let lighting = renderer.load_probe_lighting(ProbeLighting {
            diffuse: include_diffuse.then_some(&diffuse),
            reflections: include_reflections.then_some(&reflections),
        })?;
        let mut frame = renderer.begin_frame()?;
        frame.draw_scene(
            SceneInput::new(
                &meshes,
                &materials,
                if include_reflections {
                    &objects[..1]
                } else {
                    &objects[1..2]
                },
            )
            .with_baked_lighting(&lighting, 1.),
            1.,
        )?;
        let capture = frame.capture()?;
        let mut pixels = [0; 32 * 32 * 4];
        assert!(renderer.read_capture(&capture, &mut pixels, Duration::from_secs(15))?);
        let pixel = &pixels[(16 * 32 + 16) * 4..][..3];
        assert!(
            pixel[0] > 50 && pixel[0] > pixel[2].saturating_add(20),
            "{pixel:?}"
        );
    }
    // Different surface and volume keys detect both a chart lookup fallback and
    // an accidental interpolation of the two endpoints at the middle key.
    let red = [1., 0., 0.];
    let green = [0., 1., 0.];
    let blue = [0., 0., 1.];
    // Adjacent rectangles for two placements of the same mesh and material.
    let mut file = renderer::pbr::SurfaceLightmaps::MAGIC.to_vec();
    for v in [3u32, 1, 2, 8, 4, 40, 0, 0, 1, 1, 2, 2, 41, 0, 0, 5, 1, 2, 2] {
        file.extend(v.to_le_bytes());
    }
    for _y in 0..4 {
        for x in 0..8 {
            let keys = if x < 4 {
                [red, green, blue]
            } else {
                [green, blue, red]
            };
            for rgb in keys {
                for v in rgb {
                    file.extend(f32::to_le_bytes(v));
                }
            }
        }
    }
    renderer::pbr::BakedLighting::uniform([blue, red, green])?.write(&mut file)?;
    let sequence = renderer::pbr::SurfaceLightmaps::from_bytes(&file)?;
    let resident = renderer.load_surface_lighting(&sequence, None)?;
    for (object, channels) in [(1, [2, 0, 1]), (2, [0, 1, 2]), (3, [1, 2, 0])] {
        for (phase, expected) in [
            (0., [true, false, false]),
            (0.25, [true, true, false]),
            (0.5, [false, true, false]),
            (0.75, [false, true, true]),
            (1., [false, false, true]),
        ] {
            let mut frame = renderer.begin_frame()?;
            frame.draw_scene(
                SceneInput::new(&meshes, &materials, &objects[object..=object])
                    .with_baked_lighting(&resident, phase),
                1.,
            )?;
            let capture = frame.capture()?;
            let mut pixels = [0; 32 * 32 * 4];
            assert!(renderer.read_capture(&capture, &mut pixels, Duration::from_secs(15))?);
            let rgb = &pixels[(16 * 32 + 16) * 4..][..3];
            assert_eq!(
                channels.map(|c| rgb[c] > 32),
                expected,
                "receiver {object} at {phase}: {rgb:?}"
            );
        }
    }
    drop(resident);
    let mut runtime = renderer.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    validation::check_validation(runtime)
}
#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires Vulkan hardware"]
fn vulkan_probe_lighting() {
    draw(RuntimeConfig::Vulkan(Default::default())).unwrap();
}
#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires D3D12 hardware"]
fn d3d12_probe_lighting() {
    draw(RuntimeConfig::D3D12(Default::default())).unwrap();
}

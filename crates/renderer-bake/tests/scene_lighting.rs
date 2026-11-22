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
    let mesh = meshes.create(MeshData::pbr(&vertices, &[0, 2, 1])?)?;
    let mut materials = Materials::default();
    let mut metal = PbrParams::color([1.; 4]);
    metal.factors.double_sided = true;
    metal.factors.metallic = 1.;
    let mut matte = PbrParams::color([1.; 4]);
    matte.factors.double_sided = true;
    let mut charted = PbrParams::color([1.; 4]);
    charted.factors.lightmap = Some(renderer::pbr::LightmapBinding {
        first_chart: 0,
        first_vertex: 0,
    });
    charted.factors.double_sided = true;
    let objects = [
        RenderObject::new(mesh, materials.create_pbr(metal)?),
        RenderObject::new(mesh, materials.create_pbr(matte)?),
        RenderObject::new(mesh, materials.create_pbr(charted)?),
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
            .with_probe_lighting(&lighting, 1.),
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
    let chart = renderer::pbr::LightmapChart {
        extent: [2, 2],
        irradiance: vec![[red, green, blue]; 4],
    };
    let sequence = renderer::pbr::BakedLighting::from_lightmaps(&[chart], [blue, red, green])?;
    let resident = renderer.load_probe_lighting(ProbeLighting {
        diffuse: Some(&sequence),
        reflections: None,
    })?;
    for (object, channels) in [(1, [2, 0, 1]), (2, [0, 1, 2])] {
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
                    .with_probe_lighting(&resident, phase),
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

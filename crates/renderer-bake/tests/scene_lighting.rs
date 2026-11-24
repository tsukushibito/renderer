#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::{
    Renderer,
    core::runtime::RuntimeConfig,
    material::{Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{BakedLighting, PbrVertex, Projection, ViewInput},
    pipeline::*,
    unlit_forward::IDENTITY,
};
use renderer_bake::{BakeBackend, BakeScene, Lighting, ProbeSettings, ReflectionProbeDesc};
use std::time::Duration;

mod validation {
    use renderer::core as renderer_core;
    include!("../../renderer/examples/support/validation.rs");
}

fn center_pixel(renderer: &mut Renderer, scene: SceneInput<'_>) -> PipelineResult<[u8; 3]> {
    let mut frame = renderer.begin_frame()?;
    frame.draw_scene(scene, 1.)?;
    let capture = frame.capture()?;
    let mut pixels = [0; 32 * 32 * 4];
    assert!(renderer.read_capture(&capture, &mut pixels, Duration::from_secs(15))?);
    let at = (16 * 32 + 16) * 4;
    Ok(pixels[at..at + 3].try_into().unwrap())
}

// One leaf points to probe 1 at all corners. Probe 0 and the outside-volume
// fallback are black so a stride error or a missed lookup is observable.
fn quadratic_volume(keys: [[f32; 3]; 3]) -> PipelineResult<BakedLighting> {
    let mut texels = vec![[-1., -1., -1., 2.], [1., 2., 0., -3.]];
    texels.extend([[0.; 4]; 3]);
    texels.extend([[0.; 4], [1.; 4], [1.; 4]]);
    texels.extend([[0.; 4]; 2 + 9 * 3]);
    texels.extend([[2., 2., 2., 1.], [2., 2., 2., 0.]]);
    for [r, g, b] in keys {
        texels.extend([[0.; 4]; 4]);
        // For n = (2,3,-6)/7 the five basis values are (6,-18,-12,-5,59)/49.
        // These coefficients therefore reproduce the key color exactly.
        texels.extend([1., -1., -1., -1., 8. / 59.].map(|v| [v * r, v * g, v * b, 0.]));
    }
    Ok(BakedLighting::from_texels(
        texels
            .into_iter()
            .flatten()
            .flat_map(f32::to_le_bytes)
            .collect(),
    )?)
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
    let vertices = [[-0.8, -0.8, 0.5], [0.8, -0.8, 0.5], [0., 0.8, 0.5]].map(|p| {
        PbrVertex::new(p, [2. / 7., 3. / 7., -6. / 7.], [1., 0., 0., 1.], [0.; 2]).unwrap()
    });
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
    matte.factors.normal_scale = 0.;
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
        let pixel = center_pixel(
            &mut renderer,
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
        )?;
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
    let probe_keys = [blue, red, green];
    let phases = [0., 0.25, 0.5, 0.75, 1.];
    let reference = renderer.load_probe_lighting(ProbeLighting {
        diffuse: Some(&BakedLighting::uniform(probe_keys)?),
        reflections: None,
    })?;
    let mut reference_rgb = Vec::new();
    for phase in phases {
        reference_rgb.push(center_pixel(
            &mut renderer,
            SceneInput::new(&meshes, &materials, &objects[1..2])
                .with_baked_lighting(&reference, phase),
        )?);
    }
    drop(reference);
    quadratic_volume(probe_keys)?.write(&mut file)?;
    let sequence = renderer::pbr::SurfaceLightmaps::from_bytes(&file)?;
    let resident = renderer.load_surface_lighting(&sequence, None)?;
    for (object, channels) in [(1, [2, 0, 1]), (2, [0, 1, 2]), (3, [1, 2, 0])] {
        for ((phase, reference), expected) in phases.into_iter().zip(&reference_rgb).zip([
            [true, false, false],
            [true, true, false],
            [false, true, false],
            [false, true, true],
            [false, false, true],
        ]) {
            let rgb = center_pixel(
                &mut renderer,
                SceneInput::new(&meshes, &materials, &objects[object..=object])
                    .with_baked_lighting(&resident, phase),
            )?;
            if object == 1 {
                assert!(
                    rgb.iter().zip(reference).all(|(a, b)| a.abs_diff(*b) <= 1),
                    "quadratic probe at {phase}: {rgb:?} expected {reference:?}"
                );
            }
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

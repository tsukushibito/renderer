#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use sashiko_lighting_bake::{BakeBackend, BakeScene, Lighting, ProbeSettings, ReflectionProbeDesc};
use sashiko_renderer::{
    Renderer,
    core::runtime::RuntimeConfig,
    material::{Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{BakedLighting, PbrVertex, Projection, ViewInput},
    pipeline::*,
    unlit_forward::IDENTITY,
};
use sha2::{Digest, Sha256};
use std::time::Duration;

mod validation {
    use sashiko_renderer::core as sashiko_renderer_core;
    include!("../../sashiko-renderer/examples/support/validation.rs");
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
    let diffuse = sashiko_renderer::pbr::BakedLighting::from_bytes(&file)?;
    assert_eq!(diffuse.scenario_count(), 1);
    file.clear();
    reflections.write(&mut file)?;
    let reflections = sashiko_renderer::pbr::ReflectionProbes::from_bytes(&file)?;
    assert_eq!(reflections.extent(), [128, 132]);
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
    let translated = sashiko_renderer::pbr::Transform::new([
        [1., 0., 0., 0.03],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
    ])?;
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
        SceneAssets::new(&mut meshes, &mut materials, &objects).with_indirect_shadow_capacity(2),
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
    // A constant red block from ispc-texcomp's BC6H encoder. Exercise the
    // same public archive loader, owner lifetime and shading path as a cooked scene.
    let red_block = [
        194, 123, 0, 0, 8, 0, 0, 0, 128, 0, 188, 109, 219, 182, 109, 219,
    ];
    let blocks = red_block.repeat(128 * 132 / 16);
    let asset = sashiko_renderer::pbr::ReflectionProbes::new(
        reflections.probes().to_vec(),
        1,
        sashiko_renderer::core::ImageFormat::Bc6hRgbUfloat,
        blocks,
    )?;
    let mut file = Vec::new();
    asset.write(&mut file)?;
    let asset = sashiko_renderer::pbr::ReflectionProbes::from_bytes(&file)?;
    let mut reference_pixel: Option<[u8; 3]> = None;
    for probe in [&reflections, &asset] {
        let lighting = renderer.load_probe_lighting(ProbeLighting {
            diffuse: None,
            reflections: Some(probe),
        })?;
        let rgb = center_pixel(
            &mut renderer,
            SceneInput::new(&meshes, &materials, &objects[..1]).with_baked_lighting(&lighting, 0.),
        )?;
        assert!(rgb[0] > 50 && rgb[0] > rgb[2].saturating_add(20), "{rgb:?}");
        if let Some(reference) = reference_pixel {
            assert!(
                rgb.iter().zip(reference).all(|(a, b)| a.abs_diff(b) <= 1),
                "{rgb:?} vs {reference:?}"
            );
        }
        reference_pixel = Some(rgb);
    }
    // Different surface and volume keys detect both a chart lookup fallback and
    // an accidental interpolation of the two endpoints at the middle key.
    let red = [1., 0., 0.];
    let green = [0., 1., 0.];
    let blue = [0., 0., 1.];
    // Independent pages for two placements of the same mesh and material.
    let mut pages = std::collections::BTreeMap::new();
    let mut records = Vec::new();
    for (placement, colors) in [(40, [red, green, blue]), (41, [green, blue, red])] {
        let mut texels: Vec<f32> = vec![1., 3., 3., 0., 5., 4., 5., 4., 2., 2., 0., 0.];
        for _ in 0..16 {
            for [r, g, b] in colors {
                texels.extend([r, g, b, 0.]);
            }
        }
        texels.resize(8 * 7 * 4, 0.);
        let bytes: Vec<_> = texels.into_iter().flat_map(f32::to_le_bytes).collect();
        let record = sashiko_renderer::pbr::SurfacePageRecord::from_texels(
            3,
            [8, 7],
            vec![placement],
            &bytes,
        )?;
        pages.insert(*record.sha256(), bytes);
        records.push(record);
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
    let mut probes = Vec::new();
    quadratic_volume(probe_keys)?.write(&mut probes)?;
    let index = sashiko_renderer::pbr::SurfaceLightmapIndex::new(
        3,
        records,
        Sha256::digest(&probes).into(),
    )?;
    let index = sashiko_renderer::pbr::SurfaceLightmapIndex::from_bytes(&index.to_bytes())?;
    let sequence = index.load([0, 1], Some(&probes), |hash| Ok(pages[hash].clone()))?;
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
    // A black caster covers the sampled point on both static placements, including
    // the model-space variant. Duplicate capsules must not attenuate twice.
    let mut uniform = Vec::new();
    BakedLighting::uniform([[1.; 3]; 3])?.write(&mut uniform)?;
    let ambient = sashiko_renderer::pbr::SurfaceLightmapIndex::new(
        3,
        index.pages().to_vec(),
        Sha256::digest(&uniform).into(),
    )?
    .load([0, 1], Some(&uniform), |hash| Ok(pages[hash].clone()))?;
    let ambient = renderer.load_surface_lighting(&ambient, None)?;
    let caster =
        sashiko_renderer::pbr::ShadowCapsule::new([0.04, 0.04, 0.32], [0.04, 0.04, 0.32], 0.18)?;
    let far = sashiko_renderer::pbr::ShadowCapsule::new([4.; 3], [4.; 3], 0.18)?;
    for object in &objects[2..] {
        let scene = SceneInput::new(&meshes, &materials, std::slice::from_ref(object))
            .with_baked_lighting(&ambient, 0.);
        let mut pixel = |capsules: &[sashiko_renderer::pbr::ShadowCapsule]| {
            let mut input = scene;
            input.indirect_shadow_capsules = capsules;
            center_pixel(&mut renderer, input)
        };
        let base = pixel(&[])?;
        let covered = pixel(&[caster])?;
        assert!(
            covered.iter().zip(base).any(|(a, b)| *a < b)
                && covered.iter().zip(base).all(|(a, b)| *a <= b),
            "{covered:?} vs {base:?}"
        );
        assert_eq!(covered, pixel(&[caster, caster])?);
        assert_eq!(base, pixel(&[far])?);
        assert!(pixel(&[caster; 3]).is_err());
        assert_eq!(base, pixel(&[])?);
    }
    drop(ambient);
    let mut read_bytes = 0;
    let subset = index.load([1], None, |hash| {
        read_bytes += pages[hash].len();
        Ok(pages[hash].clone())
    })?;
    assert_eq!(read_bytes, 8 * 7 * 16);
    assert!(subset.probes().is_none());
    let partial = renderer.load_surface_lighting(&subset, None)?;
    // The second page now occupies local page index zero.
    for (phase, channel) in [(0., 1), (0.5, 2), (1., 0)] {
        let rgb = center_pixel(
            &mut renderer,
            SceneInput::new(&meshes, &materials, &objects[3..4])
                .with_baked_lighting(&partial, phase),
        )?;
        assert!(
            rgb[channel] > 32 && rgb[(channel + 1) % 3] <= 32 && rgb[(channel + 2) % 3] <= 32,
            "partial surface at {phase}: {rgb:?}"
        );
    }
    drop(partial);
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

//! Real device comparison of surface irradiance and both probe orders at 1–3 keys.
#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]

use renderer_bake::{
    BakeBackend, BakeScene, Lighting, LightmapVertex, Material, ProbeSettings, ProbeShOrder,
    StaticReceiver, SurfaceLayout, Triangle,
};
use sha2::{Digest, Sha256};

fn compare(backend: BakeBackend) {
    let mut scene = BakeScene::new(
        vec![Triangle {
            positions: [[-1.3, 1., -1.2], [1.7, 1., -1.2], [0.1, 1., 1.7]],
            uv: [[0.; 2]; 3],
            material: 0,
        }],
        vec![Material {
            diffuse: [0.4, 0.1, 0.1],
            emissive_nits: [0.6, 0.2, 0.],
            ..Default::default()
        }],
        vec![
            renderer_bake::Texture::new(
                4,
                2,
                (0..8).map(|i| [0.1 * i as f32, 0.3, 0.7, 1.]).collect(),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    scene.set_point_lights(vec![
        renderer_bake::PointLight::from_candela([0., 0.2, 0.], 8., [1., 0.6, 0.2], 2.).unwrap(),
    ]);
    scene.set_rect_lights(vec![
        renderer_bake::RectLight::from_lumens(
            [0., 0.25, 0.],
            [0.4, 0., 0.],
            [0., 0., -0.25],
            [0.2, 0.6, 1.],
            2.,
            6.,
        )
        .unwrap(),
    ]);
    let lights = [[0.25, 0.5, 0.75], [0.1, 0.05, 0.02]].map(|sky| Lighting {
        environment: Some(renderer_bake::SkyMap {
            texture: 0,
            yaw_radians: 0.4,
        }),
        sky_rgb_nits: sky,

        sun: renderer::pbr::Sun::from_lux([0., 1., 0.], [1.; 3], 1.).unwrap(),
    });
    let receivers = [StaticReceiver {
        placement: 42,
        extent: [8, 7],
        double_sided: true,
        material: 0,
        indices: vec![0, 1, 2],
        vertices: [[-1., 0., -1.], [1., 0., -1.], [0., 0., 1.]]
            .into_iter()
            .zip([[0., 0.], [1., 0.], [0., 1.]])
            .map(|(position, lightmap_uv)| LightmapVertex {
                position,
                normal: [0., 1., 0.],
                material_uv: [0.; 2],
                lightmap_uv,
            })
            .collect(),
    }];
    compare_keys(&scene, &receivers, backend, [lights[1]]);
    compare_keys(&scene, &receivers, backend, lights);
    compare_keys(
        &scene,
        &receivers,
        backend,
        [
            lights[0],
            Lighting::uniform_sky([0.07, 0.9, 0.04]),
            lights[1],
        ],
    );
}

fn compare_keys<const N: usize>(
    scene: &BakeScene,
    receivers: &[StaticReceiver],
    backend: BakeBackend,
    lights: [Lighting; N],
) {
    let bake = |backend, sh_order| {
        let settings = ProbeSettings {
            backend,
            sh_order,
            rays_per_probe: 64,
            max_depth: 1,
            ..Default::default()
        };
        let layout = SurfaceLayout::new(receivers, 16, N, None).unwrap();
        let selected: Vec<_> = (0..layout.gpu_page_count()).collect();
        let mut pages = std::collections::BTreeMap::new();
        let mut records = Vec::new();
        layout
            .bake_gpu_pages(
                scene.prepare_surface_bake(lights, backend, 64).unwrap(),
                &selected,
                |_, record, bytes| {
                    pages.insert(*record.sha256(), bytes);
                    records.push(record);
                    Ok(())
                },
            )
            .unwrap();
        let mut probes = Vec::new();
        scene
            .prepare_probe_bake(lights, settings)
            .unwrap()
            .bake()
            .unwrap()
            .write(&mut probes)
            .unwrap();
        let index =
            renderer::pbr::SurfaceLightmapIndex::new(N, records, Sha256::digest(&probes).into())
                .unwrap();
        let loaded = index
            .load(selected, Some(&probes), |hash| Ok(pages[hash].clone()))
            .unwrap();
        assert_eq!(
            layout.resident_bytes(),
            loaded
                .pages()
                .iter()
                .map(|p| p.rgba32f().len() as u64)
                .sum::<u64>()
        );
        loaded
    };
    let [l1, l2] = [ProbeShOrder::L1, ProbeShOrder::L2].map(|order| {
        let reference = bake(BakeBackend::Cpu, order);
        let native = bake(backend, order);
        assert_eq!(reference.placements(), native.placements());
        assert_eq!(reference.pages().len(), native.pages().len());
        for (a, b) in reference.pages().iter().zip(native.pages()) {
            assert_eq!(a.extent(), b.extent());
            for (a, b) in a
                .rgba32f()
                .as_chunks::<4>()
                .0
                .iter()
                .zip(b.rgba32f().as_chunks::<4>().0)
            {
                assert!((f32::from_le_bytes(*a) - f32::from_le_bytes(*b)).abs() < 0.0005);
            }
        }
        let (cpu_probes, gpu_probes) = (reference.probes().unwrap(), native.probes().unwrap());
        assert_eq!(gpu_probes.sh_order(), order);
        assert_eq!(gpu_probes.scenario_count(), N);
        assert_eq!(cpu_probes.extent(), gpu_probes.extent());
        for (i, (a, b)) in cpu_probes
            .rgba32f()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(gpu_probes.rgba32f().as_chunks::<4>().0)
            .enumerate()
        {
            let (a, b) = (f32::from_le_bytes(*a), f32::from_le_bytes(*b));
            assert!(
                (a - b).abs() < 0.0005,
                "{backend:?} {order:?} {N} keys, atlas word {i}: CPU {a}, GPU {b}"
            );
        }
        native
    });
    // The probe order has no effect on irradiance stored for static receivers.
    for (a, b) in l1.pages().iter().zip(l2.pages()) {
        assert_eq!(a.rgba32f(), b.rgba32f());
    }
    // This environment is directional. Inspect the high bands in every key,
    // including the unpaired last key written by a second GPU dispatch.
    let (words, _) = l2.probes().unwrap().rgba32f().as_chunks::<4>();
    let nodes = f32::from_le_bytes(words[4]) as usize;
    let probes = f32::from_le_bytes(words[5]) as usize;
    let header = 2 + N.max(2) + 3 * nodes;
    for key in 0..N {
        assert!(
            (0..probes).any(|probe| {
                let quadratic = header + probe * (2 + 9 * N) + 2 + key * 9 + 4;
                words[4 * quadratic..4 * (quadratic + 5)]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|texel| {
                        texel[..3]
                            .iter()
                            .any(|word| f32::from_le_bytes(*word).abs() > 0.0005)
                    })
            }),
            "no directional quadratic signal in key {key} of {N}"
        );
    }
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a real Vulkan device"]
fn vulkan_matches_cpu() {
    compare(BakeBackend::Vulkan);
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a real D3D12 device"]
fn d3d12_matches_cpu() {
    compare(BakeBackend::D3D12);
}

fn check_visibility(backend: BakeBackend) {
    use renderer_bake::{SurfaceSample, Texture};
    let receiver = SurfaceSample {
        position: [0.; 3],
        normal: [0., 1., 0.],
    };
    let settings = ProbeSettings {
        backend,
        rays_per_probe: 1,
        ..Default::default()
    };
    // The single hemisphere ray passes through (-0.5,0.5,0) and (-1,1,0).
    let tri = |x: f32, y: f32, r: f32, material| Triangle {
        positions: [[x - r, y, -r], [x + r, y, -r], [x, y, r]],
        uv: [[0.; 2]; 3],
        material,
    };
    let lighting = Lighting {
        sun: renderer::pbr::Sun::from_lux([0., 1., 0.], [1.; 3], 0.).unwrap(),
        sky_rgb_nits: [0.; 3],
        environment: None,
    };
    let emitter = |rgb| Material {
        diffuse: [0.; 3],
        emissive_nits: rgb,
        ..Default::default()
    };
    for alpha in [0., 1.] {
        let texture = || Texture::new(1, 1, vec![[1., 1., 1., alpha]]).unwrap();
        let mask = Material {
            base_texture: Some(0),
            alpha_cutoff: Some(0.5),
            ..emitter([0., 1., 0.])
        };
        let scene = BakeScene::new(
            vec![tri(-2., 2., 2., 0), tri(-0.5, 0.5, 0.25, 1)],
            vec![emitter([1., 0., 0.]), mask],
            vec![texture()],
        )
        .unwrap();
        let got = scene
            .sample_surface_irradiance(&[receiver], lighting, settings)
            .unwrap()[0];
        let pi = std::f32::consts::PI;
        let expected = if alpha == 0. {
            [pi, 0., 0.]
        } else {
            [0., pi, 0.]
        };
        assert!(
            got.iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 0.0005),
            "{got:?}"
        );
        // The small caster misses the incoming ray, but can block the point light.
        for (height, blocked) in [(0.5, alpha == 1.), (-0.5, false)] {
            let mut scene = BakeScene::new(
                vec![tri(-1., 1., 0.4, 0), tri(-1., height, 0.15, 1)],
                vec![
                    Material {
                        diffuse: [1.; 3],
                        ..Default::default()
                    },
                    Material {
                        emissive_nits: [0.; 3],
                        ..mask
                    },
                ],
                vec![texture()],
            )
            .unwrap();
            scene.set_point_lights(vec![
                renderer_bake::PointLight::from_candela([-1., 0., 0.], 8., [1.; 3], 1.).unwrap(),
            ]);
            let expected = scene.bake_irradiance(&[receiver], lighting, 1).unwrap()[0];
            let got = scene
                .sample_surface_irradiance(&[receiver], lighting, settings)
                .unwrap()[0];
            assert_eq!(expected[0] < 0.0005, blocked);
            assert!(
                got.iter()
                    .zip(expected)
                    .all(|(a, b)| (a - b).abs() < 0.0005),
                "{got:?} {expected:?}"
            );
        }
    }
    // The mask and emitter use the same texels with different wraps and filters.
    // Only nearest, U clamp + V mirror reaches the opaque texel. Linear repeat
    // samples red = 0.35 at (-0.125, 1.125) in the same 2x2 texture.
    use renderer::core::program::{AddressMode, FilterMode, SamplerDescriptor};
    let mut front = tri(-0.5, 0.5, 0.25, 0);
    front.uv = [[-0.125, 1.125]; 3];
    let mut m = Material {
        diffuse: [0.; 3],
        emissive_nits: [1.; 3],
        alpha_cutoff: Some(0.9),
        base_texture: Some(0),
        emission_texture: Some(0),
        ..Default::default()
    };
    m.samplers.base_color = SamplerDescriptor {
        wrap_u: AddressMode::ClampToEdge,
        wrap_v: AddressMode::MirroredRepeat,
        mag_filter: FilterMode::Nearest,
        ..Default::default()
    };
    let scene = BakeScene::new(
        vec![front],
        vec![m],
        vec![
            Texture::new(
                2,
                2,
                vec![
                    [0.; 4],
                    [0.4, 0., 0., 0.],
                    [0.2, 0., 0., 1.],
                    [0.6, 0., 0., 0.],
                ],
            )
            .unwrap(),
        ],
    )
    .unwrap();
    for actual in [
        scene.bake_irradiance(&[receiver], lighting, 1).unwrap()[0],
        scene
            .sample_surface_irradiance(&[receiver], lighting, settings)
            .unwrap()[0],
    ] {
        assert!(
            (actual[0] - 0.35 * std::f32::consts::PI).abs() < 0.0005,
            "{actual:?}"
        );
    }
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires Vulkan ray queries and a Vulkan compute device"]
fn vulkan_visibility_masks_and_distances() {
    check_visibility(BakeBackend::VulkanRayQuery);
    check_visibility(BakeBackend::VulkanSoftware);
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires D3D12"]
fn d3d12_visibility_masks_and_distances() {
    check_visibility(BakeBackend::D3D12);
}

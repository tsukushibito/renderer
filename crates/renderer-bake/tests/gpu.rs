//! Real device comparison covering both receiver types and both lighting scenarios.
#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]

use renderer_bake::{
    BakeBackend, BakeScene, Lighting, LightmapVertex, Material, ProbeSettings, StaticReceiver,
    SurfaceLayout, Triangle,
};

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
    fn bake<const N: usize>(
        scene: &BakeScene,
        receivers: &[StaticReceiver],
        backend: BakeBackend,
        lights: impl renderer_bake::LightingScenarios<N>,
    ) -> renderer::pbr::SurfaceLightmaps {
        let mut file = Vec::new();
        let layout = SurfaceLayout::new(receivers, 16, N, None).unwrap();
        layout
            .bake_to(
                scene,
                lights,
                ProbeSettings {
                    backend,
                    rays_per_probe: 64,
                    max_depth: 1,
                    ..Default::default()
                },
                64,
                &mut file,
            )
            .unwrap();
        let uploaded = renderer::pbr::SurfaceLightmaps::from_bytes(&file).unwrap();
        let bytes: u64 = uploaded
            .pages()
            .iter()
            .map(|p| p.rgba32f().len() as u64)
            .sum();
        assert_eq!(layout.resident_bytes(), bytes);
        assert_eq!(layout.gpu_image_count(), uploaded.pages().len());
        uploaded
    }
    let verify = |reference: renderer::pbr::SurfaceLightmaps,
                  native: renderer::pbr::SurfaceLightmaps| {
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
        let (reference, native) = (reference.probes(), native.probes());
        assert_eq!(reference.extent(), native.extent());
        for (i, (a, b)) in reference
            .rgba32f()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(native.rgba32f().as_chunks::<4>().0)
            .enumerate()
        {
            let (a, b) = (f32::from_le_bytes(*a), f32::from_le_bytes(*b));
            assert!(
                (a - b).abs() < 0.0005,
                "{backend:?} atlas word {i}: CPU {a}, GPU {b}"
            );
        }
    };
    verify(
        bake(&scene, &receivers, BakeBackend::Cpu, lights),
        bake(&scene, &receivers, backend, lights),
    );
    verify(
        bake(&scene, &receivers, BakeBackend::Cpu, lights[1]),
        bake(&scene, &receivers, backend, lights[1]),
    );
    let keys = [
        lights[0],
        Lighting::uniform_sky([0.07, 0.9, 0.04]),
        lights[1],
    ];
    verify(
        bake(&scene, &receivers, BakeBackend::Cpu, keys),
        bake(&scene, &receivers, backend, keys),
    );
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

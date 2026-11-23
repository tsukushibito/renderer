//! Real device comparison covering both receiver types and both lighting scenarios.
#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]

use renderer_bake::{
    BakeBackend, BakeScene, Lighting, LightmapSettings, Material, ProbeSettings, ReceiverTriangle,
    Triangle,
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
            emission: [0.6, 0.2, 0.],
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
    scene.set_rect_lights(vec![
        renderer_bake::RectLight::new(
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
        sky,
        direction_to_sun: [0., 1., 0.],
        sun: [1.; 3],
    });
    let receivers = [ReceiverTriangle {
        positions: [[-1., 0., -1.], [1., 0., -1.], [0., 0., 1.]],
        normals: [[0., 1., 0.]; 3],
    }];
    fn bake<const N: usize>(
        scene: &BakeScene,
        receivers: &[ReceiverTriangle],
        backend: BakeBackend,
        lights: impl renderer_bake::LightingScenarios<N> + Copy,
    ) -> renderer::pbr::BakedLighting {
        let probes = scene
            .bake_probes(
                lights,
                ProbeSettings {
                    backend,
                    rays_per_probe: 64,
                    max_depth: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        let charts = scene
            .bake_lightmaps(
                receivers,
                lights,
                LightmapSettings {
                    backend,
                    rays_per_texel: 64,
                    ..Default::default()
                },
            )
            .unwrap();
        probes.with_lightmaps(&charts).unwrap()
    }
    let verify = |reference: renderer::pbr::BakedLighting, native: renderer::pbr::BakedLighting| {
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

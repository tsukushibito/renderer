//! Public PBR recipe contracts and opt-in tests for native execution.

#[path = "../examples/support/pbr_scene.rs"]
mod scene;

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "../examples/support/pbr_runtime.rs"]
mod runtime;

mod cpu {
    use std::num::NonZeroUsize;

    use renderer::graph::{GraphCapacity, GraphPool, SampleCount};
    use renderer_pipeline::{passes::bloom::Bloom, pbr::CASCADE_COUNT};

    use super::scene::*;

    #[test]
    fn application_copy_is_between_shading_and_sdr_conversion() {
        let triangle = Triangle::new(false, Placement::Single).unwrap();
        let mut prepared = prepare(NonZeroUsize::MIN, Settings::new(SampleCount::X1)).unwrap();
        for (copy_hdr, bloom) in [(false, Bloom::Off), (true, Bloom::Off), (true, Bloom::Tent)] {
            let (preparation, post) = reserve(&prepared, &triangle, copy_hdr, bloom).unwrap();
            assert!(resource_count(&preparation) >= 10);
            let capacity = preparation
                .graph_capacity(GraphCapacity::default())
                .unwrap();
            let mut pool = GraphPool::new(capacity, 1).unwrap();
            let (compiled, readbacks, _) = build_graph(
                &mut pool,
                &mut prepared,
                &triangle,
                None,
                &post,
                Placement::Single,
                None,
            )
            .unwrap();
            let depth = readbacks.depth.expect("1× exposes scene depth");
            assert_ne!(readbacks.sdr, depth);
            let passes = compiled.ordered_pass_names().collect::<Vec<_>>();
            let tone_map = passes.iter().position(|&name| name == "tone_map").unwrap();
            let write_sdr = passes.iter().position(|&name| name == "write_sdr").unwrap();
            assert!(tone_map < write_sdr);
            if copy_hdr {
                let position = passes.iter().position(|&name| name == COPY_PASS).unwrap();
                assert!(position < tone_map);
                if bloom != Bloom::Off {
                    let combine = passes
                        .iter()
                        .position(|&name| name == "Bloom combine")
                        .unwrap();
                    assert!(position < combine && combine < tone_map);
                }
            } else {
                assert!(!passes.contains(&COPY_PASS));
            }
            assert!(passes.contains(&"read SDR") && passes.contains(&"read depth"));
        }
    }

    #[test]
    fn four_samples_resolve_color_without_a_depth_readback() {
        let triangle = Triangle::new(false, Placement::Single).unwrap();
        let mut prepared = prepare(NonZeroUsize::MIN, Settings::new(SampleCount::X4)).unwrap();
        let (preparation, post) = reserve(&prepared, &triangle, false, Bloom::Off).unwrap();
        let capacity = preparation
            .graph_capacity(GraphCapacity::default())
            .unwrap();
        let mut pool = GraphPool::new(capacity, 1).unwrap();
        let (compiled, readbacks, _) = build_graph(
            &mut pool,
            &mut prepared,
            &triangle,
            None,
            &post,
            Placement::Single,
            None,
        )
        .unwrap();
        assert!(compiled.ordered_pass_names().any(|name| name == "read SDR"));
        assert!(readbacks.depth.is_none());
    }

    #[test]
    fn transformed_scene_declares_all_cascades() {
        for placement in [Placement::Individual, Placement::Tilted(Some(-40))] {
            let settings = Settings {
                samples: SampleCount::X1,
                placement,
            };
            let triangle = Triangle::new(false, placement).unwrap();
            let mut prepared = prepare(NonZeroUsize::MIN, settings).unwrap();
            let (preparation, post) = reserve(&prepared, &triangle, false, Bloom::Off).unwrap();
            let capacity = preparation
                .graph_capacity(GraphCapacity::default())
                .unwrap();
            let mut pool = GraphPool::new(capacity, 1).unwrap();
            let (compiled, readbacks, _) = build_graph(
                &mut pool,
                &mut prepared,
                &triangle,
                None,
                &post,
                placement,
                None,
            )
            .unwrap();
            assert!(readbacks.shadow_maps.iter().all(Option::is_some));
            assert_eq!(
                compiled
                    .ordered_pass_names()
                    .filter(|name| name.starts_with("shadow_"))
                    .count(),
                CASCADE_COUNT,
            );
        }
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
mod gpu {
    use renderer::graph::SampleCount;
    use renderer_pipeline::passes::bloom::Bloom;

    use super::{
        runtime::{Api, probe, render},
        scene::{EXTENT, Placement, Settings},
    };

    fn compare(api: Api) {
        for samples in [SampleCount::X1, SampleCount::X4] {
            let settings = Settings::new(samples);
            let full = render(api, settings, false, Bloom::Off, &[]).unwrap();
            let split = render(api, settings, true, Bloom::Off, &[]).unwrap();
            let (_, depth) = probe(&full[0].output).unwrap();
            assert_eq!(depth.is_some(), samples == SampleCount::X1);
            assert_eq!(full[0].output, split[0].output);
        }
    }

    fn bloom_and_history(api: Api) {
        let draws = render(
            api,
            Settings::new(SampleCount::X1),
            true,
            Bloom::Tent,
            &[[0; 4], [0; 4], [0, 0, 1, 0]],
        )
        .unwrap();
        assert_eq!(draws[0].reused, [false; 4]);
        assert_eq!(draws[1].reused, [true; 4]);
        assert_eq!(draws[2].reused, [true, true, false, true]);
        assert_eq!(draws[0].shadow_passes, 4);
        assert_eq!(draws[1].shadow_passes, 0);
        assert_eq!(draws[2].shadow_passes, 1);
        assert_eq!(draws[0].output, draws[1].output);
        assert_eq!(draws[0].output, draws[2].output);

        let image = &draws[0].output;
        probe(image).unwrap();
        // The HDR emitter spreads color into at least one background pixel.
        assert!(
            image
                .sdr
                .as_chunks::<4>()
                .0
                .iter()
                .zip(image.depth.as_ref().unwrap().as_chunks::<4>().0)
                .any(|(effect, depth)| *depth == 1.0_f32.to_le_bytes()
                    && effect[0] > 0
                    && effect[3] == 255)
        );
    }

    fn transformed_instances_match_individual_draws(api: Api) {
        let render = |placement| {
            let settings = Settings {
                samples: SampleCount::X1,
                placement,
            };
            render(api, settings, false, Bloom::Off, &[]).unwrap()
        };
        let individual = render(Placement::Individual);
        let instanced = render(Placement::Instanced);
        let output = &instanced[0].output;
        assert_eq!(individual[0].output, *output);

        // Both objects must contribute distinct covered pixels.
        let depth = output.depth.as_ref().unwrap();
        for x in [25, 39] {
            let offset = (26 * EXTENT[0] as usize + x) * 4;
            let rgba: [u8; 4] = output.sdr[offset..offset + 4].try_into().unwrap();
            assert!(matches!(rgba, [r, g, b, 255] if r > 80 && r > g && r > b));
            let z = f32::from_le_bytes(depth[offset..offset + 4].try_into().unwrap());
            assert!((0.0..1.0).contains(&z));
        }
        assert!(output.shadow_maps.iter().any(|image| {
            image
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| *pixel != 1.0_f32.to_le_bytes())
        }));
    }

    fn extreme_scales_preserve_normal_direction(api: Api) {
        let capture = |placement| {
            let settings = Settings {
                samples: SampleCount::X1,
                placement,
            };
            render(api, settings, false, Bloom::Off, &[])
                .unwrap()
                .pop()
                .unwrap()
                .output
        };
        let center = ((EXTENT[1] / 2) * EXTENT[0] + EXTENT[0] / 2) as usize * 4;
        let upright = capture(Placement::Single);
        let reference = capture(Placement::Tilted(None));
        assert!(reference.sdr[center] > 60);
        assert!(reference.sdr[center].abs_diff(upright.sdr[center]) > 8);

        // Inverse powers of two preserve the world-space vertex positions
        // exactly while the transformed normal crosses the length limits.
        for exponent in [-40, 40] {
            let scaled = capture(Placement::Tilted(Some(exponent)));
            assert_eq!(reference.depth, scaled.depth, "scale 2^{exponent}");
            assert_eq!(
                reference.shadow_maps, scaled.shadow_maps,
                "scale 2^{exponent}"
            );
            let expected = &reference.sdr[center..center + 4];
            let actual = &scaled.sdr[center..center + 4];
            assert!(
                expected
                    .iter()
                    .zip(actual)
                    .all(|(&a, &b)| a.abs_diff(b) <= 1),
                "scale 2^{exponent}: expected {expected:?}, got {actual:?}",
            );
        }
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
    fn vulkan_full_and_split_recipes_match() {
        compare(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
    fn d3d12_full_and_split_recipes_match() {
        compare(Api::D3D12);
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
    fn vulkan_bloom_and_shadow_history() {
        bloom_and_history(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
    fn d3d12_bloom_and_shadow_history() {
        bloom_and_history(Api::D3D12);
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
    fn vulkan_transformed_casters_match_instancing() {
        transformed_instances_match_individual_draws(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
    fn d3d12_transformed_casters_match_instancing() {
        transformed_instances_match_individual_draws(Api::D3D12);
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
    fn vulkan_extreme_scales_preserve_normal_direction() {
        extreme_scales_preserve_normal_direction(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
    fn d3d12_extreme_scales_preserve_normal_direction() {
        extreme_scales_preserve_normal_direction(Api::D3D12);
    }
}

//! Public PBR recipe contracts and opt-in tests for native execution.

#[path = "../examples/support/pbr_scene.rs"]
mod scene;

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "../examples/support/pbr_runtime.rs"]
mod runtime;

#[cfg(all(
    feature = "memory-profiling",
    any(feature = "vulkan", all(windows, feature = "d3d12"))
))]
#[global_allocator]
static ALLOCATOR: renderer::measurement::CountingAllocator<std::alloc::System> =
    renderer::measurement::CountingAllocator::new(std::alloc::System);

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "pbr_gpu/mixed.rs"]
mod mixed;
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
            assert!(preparation.resource_count().unwrap() >= 10);
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
                Settings::new(SampleCount::X1),
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
    fn color_resolve_and_depth_readback_follow_the_technique() {
        for (technique, samples, view) in [
            (Technique::Forward, SampleCount::X4, ViewFixture::Centered),
            (
                Technique::ForwardPlus,
                SampleCount::X4,
                ViewFixture::Centered,
            ),
            (
                Technique::Deferred,
                SampleCount::X1,
                ViewFixture::OverheadOrthographic,
            ),
            (
                Technique::Deferred,
                SampleCount::X1,
                ViewFixture::OffsetPerspective,
            ),
            (
                Technique::Deferred,
                SampleCount::X1,
                ViewFixture::LeftHandedPerspective {
                    double_sided: false,
                },
            ),
        ] {
            let settings = Settings {
                technique,
                view,
                ..Settings::new(samples)
            };
            let triangle = Triangle::for_view(false, settings.placement, settings.view).unwrap();
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
                settings,
                None,
            )
            .unwrap();
            assert!(compiled.ordered_pass_names().any(|name| name == "read SDR"));
            assert_eq!(readbacks.depth.is_some(), samples == SampleCount::X1);
        }
    }

    #[test]
    fn transformed_scene_declares_all_cascades() {
        for placement in [
            Placement::Individual,
            Placement::Instanced,
            Placement::PagedInstanced,
            Placement::Tilted(Some(-40)),
            Placement::Mirrored {
                baked: false,
                blend: false,
            },
        ] {
            let settings = Settings {
                placement,
                ..Settings::new(SampleCount::X1)
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
                settings,
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
        scene::{EXTENT, Placement, Settings, Technique, ViewFixture},
    };

    use super::scene::POINT_LIGHTS;
    use renderer_pipeline::pbr::PointLight;

    const LATE_POINT_LIGHTS: [PointLight; 257] = {
        let mut lights = [PointLight {
            intensity: 0.0,
            ..POINT_LIGHTS[0]
        }; 257];
        lights[256] = POINT_LIGHTS[0];
        lights
    };

    fn late_point_light_matches_single_light(api: Api) {
        for technique in [
            Technique::Forward,
            Technique::ForwardPlus,
            Technique::Deferred,
        ] {
            for placement in [
                Placement::Single,
                Placement::Mirrored {
                    baked: false,
                    blend: true,
                },
            ] {
                let settings = Settings {
                    technique,
                    placement,
                    frame_lights: &[&[], &POINT_LIGHTS, &LATE_POINT_LIGHTS],
                    ..Settings::new(SampleCount::X1)
                };
                // All three submissions are live before any result is consumed.
                let frames = render(api, settings, false, Bloom::Off, &[]).unwrap();
                let reference = &frames[1].output.sdr;
                assert!(
                    *reference != frames[0].output.sdr,
                    "{technique:?} point light must affect shading"
                );
                assert!(
                    *reference == frames[2].output.sdr,
                    "{technique:?} light in second table row"
                );
            }
        }
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
    fn vulkan_late_point_light_matches_single_light() {
        late_point_light_matches_single_light(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
    fn d3d12_late_point_light_matches_single_light() {
        late_point_light_matches_single_light(Api::D3D12);
    }

    fn compare(api: Api) {
        let mut forward_red = 0;
        for technique in [
            Technique::Forward,
            Technique::ForwardPlus,
            Technique::Deferred,
        ] {
            for samples in [SampleCount::X1, SampleCount::X4] {
                if matches!(technique, Technique::Deferred) && samples == SampleCount::X4 {
                    continue;
                }
                let settings = Settings {
                    technique,
                    ..Settings::new(samples)
                };
                let full = render(api, settings, false, Bloom::Off, &[]).unwrap();
                let split = render(api, settings, true, Bloom::Off, &[]).unwrap();
                let ([red, ..], depth) = probe(&full[0].output).unwrap();
                assert_eq!(depth.is_some(), samples == SampleCount::X1);
                assert_eq!(
                    full[0].output, split[0].output,
                    "{technique:?}, {samples:?}"
                );
                if samples == SampleCount::X1 {
                    match technique {
                        Technique::Forward => forward_red = red,
                        Technique::ForwardPlus => assert!(red > forward_red),
                        Technique::Deferred => {}
                    }
                }
            }
        }
    }

    fn application_projection_matches_analytic_samples(api: Api) {
        // For the overhead camera the triangle lies in the horizontal plane,
        // three units from the camera. Perspective shifts its center by 0.45
        // NDC in X while keeping its distance at three units.
        for (view, covered, outside, depth) in [
            (ViewFixture::OverheadOrthographic, 32, 46, 2.9_f32 / 9.9),
            (
                ViewFixture::OffsetPerspective,
                46,
                32,
                (2.9_f32 / 9.9) * (10.0 / 3.0),
            ),
            (
                ViewFixture::LeftHandedPerspective {
                    double_sided: false,
                },
                46,
                32,
                (2.9_f32 / 9.9) * (10.0 / 3.0),
            ),
        ] {
            for technique in [Technique::Forward, Technique::Deferred] {
                let settings = Settings {
                    technique,
                    view,
                    ..Settings::new(SampleCount::X1)
                };
                let rendered = render(api, settings, false, Bloom::Off, &[]).unwrap();
                let output = &rendered[0].output;
                let depths = output.depth.as_ref().unwrap();
                let sample = |x| {
                    let offset = ((EXTENT[1] / 2 * EXTENT[0] + x) * 4) as usize;
                    let color: [u8; 4] = output.sdr[offset..offset + 4].try_into().unwrap();
                    let depth = f32::from_le_bytes(depths[offset..offset + 4].try_into().unwrap());
                    (color, depth)
                };
                let (color, actual_depth) = sample(covered);
                assert!(
                    matches!(color, [r, g, b, 255] if r > 80 && r > g && r > b),
                    "{technique:?} covered color {color:?}"
                );
                assert!(
                    (actual_depth - depth).abs() < 2e-6,
                    "{technique:?} expected depth {depth}, got {actual_depth}"
                );
                assert_eq!(sample(outside), ([0, 0, 0, 255], 1.0));
            }
        }
    }

    fn left_handed_projection_preserves_independent_shadow_winding(api: Api) {
        for double_sided in [false, true] {
            let settings = Settings {
                view: ViewFixture::LeftHandedPerspective { double_sided },
                placement: Placement::Tilted(None),
                ..Settings::new(SampleCount::X1)
            };
            let frames = render(api, settings, false, Bloom::Off, &[]).unwrap();
            let output = &frames[0].output;
            let offset = ((EXTENT[1] / 2 * EXTENT[0] + 46) * 4) as usize;
            let rgba: [u8; 4] = output.sdr[offset..offset + 4].try_into().unwrap();
            let depth = output.depth.as_ref().unwrap();
            let depth = f32::from_le_bytes(depth[offset..offset + 4].try_into().unwrap());
            assert!(matches!(rgba, [r, g, b, 255] if r > 32 && r > g && r > b));
            assert!((depth - (2.9 / 9.9) * (10.0 / 3.0)).abs() < 2e-6);

            // The same triangle is front-facing to the view, but back-facing
            // to this sun projection. Only the two-sided mask casts depth.
            let shadow_written = output.shadow_maps.iter().any(|image| {
                image
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| *pixel != 1.0_f32.to_le_bytes())
            });
            assert_eq!(shadow_written, double_sided);
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
        let capture = |technique, placement| {
            let settings = Settings {
                technique,
                placement,
                frame_lights: &[&LATE_POINT_LIGHTS],
                ..Settings::new(SampleCount::X1)
            };
            render(api, settings, false, Bloom::Off, &[]).unwrap()
        };
        let individual = capture(Technique::Forward, Placement::Individual);
        let instanced = capture(Technique::Forward, Placement::Instanced);
        let output = &instanced[0].output;
        assert_eq!(individual[0].output, *output);
        assert_eq!(
            capture(Technique::Forward, Placement::PagedInstanced)[0].output,
            *output
        );

        for technique in [Technique::ForwardPlus, Technique::Deferred] {
            let reference = capture(technique, Placement::Individual);
            let paged = capture(technique, Placement::PagedInstanced);
            assert_eq!(paged[0].output, reference[0].output, "{technique:?}");
        }

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
                placement,
                ..Settings::new(SampleCount::X1)
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

    fn mirrored_model_draws_match_baked_geometry(api: Api) {
        for technique in [
            Technique::Forward,
            Technique::ForwardPlus,
            Technique::Deferred,
        ] {
            for blend in [false, true] {
                let capture = |baked| {
                    let settings = Settings {
                        technique,
                        placement: Placement::Mirrored { baked, blend },
                        ..Settings::new(SampleCount::X1)
                    };
                    render(api, settings, false, Bloom::Off, &[])
                        .unwrap()
                        .pop()
                        .unwrap()
                        .output
                };
                let expected = capture(true);
                let actual = capture(false);
                let center = ((EXTENT[1] / 2) * EXTENT[0] + EXTENT[0] / 2) as usize * 4;
                assert!(expected.sdr[center] > 30, "{technique:?} blend={blend}");
                assert_eq!(expected.depth, actual.depth, "{technique:?} blend={blend}");
                assert_eq!(
                    expected.shadow_maps, actual.shadow_maps,
                    "{technique:?} blend={blend}"
                );
                assert!(
                    expected
                        .sdr
                        .iter()
                        .zip(&actual.sdr)
                        .all(|(&a, &b)| a.abs_diff(b) <= 1),
                    "mirrored material differs for {technique:?} blend={blend}",
                );
            }
        }
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
    fn vulkan_mirrored_model_draws_match_baked_geometry() {
        mirrored_model_draws_match_baked_geometry(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
    fn d3d12_mirrored_model_draws_match_baked_geometry() {
        mirrored_model_draws_match_baked_geometry(Api::D3D12);
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
    fn vulkan_application_projection_matches_analytic_samples() {
        application_projection_matches_analytic_samples(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
    fn d3d12_application_projection_matches_analytic_samples() {
        application_projection_matches_analytic_samples(Api::D3D12);
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
    fn vulkan_left_handed_projection_preserves_independent_shadow_winding() {
        left_handed_projection_preserves_independent_shadow_winding(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
    fn d3d12_left_handed_projection_preserves_independent_shadow_winding() {
        left_handed_projection_preserves_independent_shadow_winding(Api::D3D12);
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

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
static ALLOCATOR: renderer_core::measurement::CountingAllocator<std::alloc::System> =
    renderer_core::measurement::CountingAllocator::new(std::alloc::System);

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "pbr_gpu/mixed.rs"]
mod mixed;
mod cpu {
    use std::num::NonZeroUsize;

    use renderer::{passes::bloom::Bloom, pbr::CASCADE_COUNT};
    use renderer_core::graph::{GraphCapacity, GraphPool, SampleCount};

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
    use renderer::passes::bloom::Bloom;
    use renderer_core::graph::SampleCount;

    use super::{
        runtime::{Api, probe, render},
        scene::{EXTENT, Placement, Settings, Technique, ViewFixture},
    };

    use super::scene::POINT_LIGHTS;
    use renderer::pbr::LocalLight;

    static LATE_POINT_LIGHTS: std::sync::LazyLock<[LocalLight; 257]> =
        std::sync::LazyLock::new(|| {
            let mut lights = [LocalLight::Point(
                renderer::pbr::PointLight::from_candela([0., 0., 1.], 3., [1.; 3], 0.).unwrap(),
            ); 257];
            lights[256] = POINT_LIGHTS[0];
            lights
        });

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
                    frame_lights: &[&[], &*POINT_LIGHTS, &*LATE_POINT_LIGHTS],
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

    fn compare_point_and_sun(api: Api) {
        // Compare the same receiver and BRDF under 4 cd at 1 and 2 metres
        // and under directional lights of 4 and 1 lux, respectively.
        let [width, height] = super::scene::EXTENT;
        let half_pixel = 3. * (55_f32.to_radians() * 0.5).tan() / height as f32;
        let pixel = ((height / 2 * width + width / 2) * 4) as usize;
        let point = |z| {
            [LocalLight::Point(
                renderer::pbr::PointLight::from_candela(
                    [half_pixel, -half_pixel, z],
                    1000.,
                    [1.; 3],
                    4.,
                )
                .unwrap(),
            )]
        };
        let (near, far) = (point(1.), point(2.));
        let image = |lux, lights: &[&[LocalLight]]| {
            render(
                api,
                Settings {
                    technique: Technique::Forward,
                    sun_lux: lux,
                    frame_lights: lights,
                    ..Settings::new(SampleCount::X1)
                },
                false,
                Bloom::Off,
                &[],
            )
            .unwrap()
        };
        let points = image(0., &[&near, &far]);
        let mut red = [0; 2];
        for (index, lux) in [4., 1.].into_iter().enumerate() {
            let sun = image(lux, &[&[]]);
            for c in 0..3 {
                let actual = points[index].output.sdr[pixel + c];
                let expected = sun[0].output.sdr[pixel + c];
                assert!(
                    actual.abs_diff(expected) <= 2,
                    "{lux} lx channel {c}: {actual} vs {expected}"
                );
            }
            red[index] = sun[0].output.sdr[pixel];
        }
        assert!(red[0] > red[1] && red[1] > 0);
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires real Vulkan GPU"]
    fn vulkan_point_sun_photometry() {
        compare_point_and_sun(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires real D3D12 GPU"]
    fn d3d12_point_sun_photometry() {
        compare_point_and_sun(Api::D3D12);
    }

    fn moving_rectangle_blocker(api: Api) {
        let rect = renderer::pbr::RectLight::from_lumens(
            [1.5, 0., 1.5],
            [0.3, 0., 0.],
            [0., -0.2, 0.],
            [1.; 3],
            80.,
            5.,
        )
        .unwrap();
        let lights = [LocalLight::shadowed_rect(rect)];
        for technique in [
            Technique::Forward,
            Technique::ForwardPlus,
            Technique::Deferred,
        ] {
            let frames = render(
                api,
                Settings {
                    technique,
                    frame_lights: &[&[], &lights, &lights, &lights],
                    blockers: &[
                        super::scene::ShadowBlocker {
                            x: 0.75,
                            cutout: false,
                        },
                        super::scene::ShadowBlocker {
                            x: 0.75,
                            cutout: false,
                        },
                        super::scene::ShadowBlocker {
                            x: 4.,
                            cutout: false,
                        },
                        super::scene::ShadowBlocker {
                            x: 0.75,
                            cutout: true,
                        },
                    ],
                    ..Settings::new(SampleCount::X1)
                },
                false,
                Bloom::Off,
                &[],
            )
            .unwrap();
            let center = |i: usize| probe(&frames[i].output).unwrap().0;
            assert_eq!(center(0), center(1), "{technique:?} covered receiver");
            assert!(
                center(2)[0] > center(1)[0],
                "{technique:?} moved blocker must reveal receiver"
            );
            assert_eq!(center(2), center(3), "{technique:?} alpha-mask hole");
            let center_index = (super::scene::EXTENT[1] / 2 * super::scene::EXTENT[0]
                + super::scene::EXTENT[0] / 2) as usize;
            // The receiver straddles the +Z/+X cube-face boundary.
            for i in center_index - 2..=center_index + 2 {
                assert_eq!(
                    frames[0].output.sdr[i * 4..i * 4 + 3],
                    frames[1].output.sdr[i * 4..i * 4 + 3]
                );
            }
            assert_eq!(frames[0].output.depth, frames[2].output.depth);
        }
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires real Vulkan GPU"]
    fn vulkan_moving_rectangle_blocker() {
        moving_rectangle_blocker(Api::Vulkan)
    }
    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires real D3D12 GPU"]
    fn d3d12_moving_rectangle_blocker() {
        moving_rectangle_blocker(Api::D3D12)
    }

    fn one_sided_rectangle(api: Api) {
        let light = |y| {
            LocalLight::Rect(
                renderer::pbr::RectLight::from_lumens(
                    [0.1, 0.3, 1.5],
                    [0.5, 0., 0.],
                    [0., y, 0.],
                    [1., 0.3, 0.1],
                    80.,
                    5.,
                )
                .unwrap(),
            )
        };
        let front = [light(-0.25)];
        let back = [light(0.25)];
        for technique in [
            Technique::Forward,
            Technique::ForwardPlus,
            Technique::Deferred,
        ] {
            let frames = render(
                api,
                Settings {
                    technique,
                    frame_lights: &[&[], &front, &back],
                    ..Settings::new(SampleCount::X1)
                },
                false,
                Bloom::Off,
                &[],
            )
            .unwrap();
            assert!(
                frames[1].output.sdr != frames[0].output.sdr,
                "{technique:?} front face"
            );
            assert!(
                frames[2].output.sdr == frames[0].output.sdr,
                "{technique:?} back face"
            );
        }
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
    fn vulkan_one_sided_rectangle() {
        one_sided_rectangle(Api::Vulkan);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
    fn d3d12_one_sided_rectangle() {
        one_sided_rectangle(Api::D3D12);
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
                frame_lights: &[&*LATE_POINT_LIGHTS],
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

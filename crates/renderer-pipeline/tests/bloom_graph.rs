//! Standard Bloom composed with uploaded HDR and scene rendering through Graph.

use renderer::capacity::{GraphRequirements, Preparation};
use renderer::graph::{CompiledGraph, GraphCapacity, GraphPool, ImageDesc, PassHandle};
use renderer_pipeline::passes::bloom::{Bloom, PreparedBloom};
use renderer_pipeline::unlit_forward::{ForwardConfig, forward};
use std::num::NonZeroUsize;

const WIDTH: u32 = 9;
const HEIGHT: u32 = 5;
const HDR_BYTES: usize = WIDTH as usize * HEIGHT as usize * 8;
const CAPACITY: GraphCapacity = GraphCapacity {
    resources: 0,
    passes: 0,
    accesses: 0,
    draws: 0,
    raster_constant_words: Some(0),
    bindings: 0,
    attachments: 0,
    dependencies: 16,
    initial_ranges: 16,
    name_bytes: 0,
    upload_bytes: HDR_BYTES,
    edges: 64,
    segments: 64,
    readers: 64,
};

fn input_desc() -> ImageDesc {
    ImageDesc::rgba16_float(WIDTH, HEIGHT)
}

fn preparation() -> Preparation {
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    preparation.reserve_image(input_desc(), 1).unwrap();
    preparation
        .add_graph_requirements(GraphRequirements {
            resources: 1,
            passes: 2,
            accesses: 2,
            attachments: 1,
            name_bytes: 64,
            ..GraphRequirements::default()
        })
        .unwrap();
    preparation
}

fn input_pixel(x: u32, y: u32, standard: bool) -> [u16; 4] {
    let red = if standard || (x < 4 && y < 4) {
        0x4c40 // 17
    } else if x == 8 && y == 4 {
        0x4880 // 9
    } else {
        0x3800 // 0.5
    };
    let green = if !standard && x < 4 && y < 4 {
        if (x + y).is_multiple_of(2) {
            0x39f0
        } else {
            0x387b
        }
    } else {
        0
    };
    [red, green, 0, 0x3a00] // alpha 0.75
}

fn fixture(
    pool: &mut GraphPool,
    effect: &PreparedBloom,
    standard: bool,
    keep_effect: bool,
) -> (CompiledGraph, PassHandle) {
    let mut graph = pool.begin().unwrap();
    let output = if standard {
        let scene = forward(
            &mut graph,
            "standard HDR clear",
            std::iter::empty::<renderer::graph::RasterDraw>(),
            ForwardConfig {
                color: input_desc(),
                clear_color: [17.0, 0.0, 0.0, 0.75],
                depth: false,
            },
        )
        .unwrap();
        let output = effect.add_passes(&mut graph, scene.color).unwrap();
        if keep_effect { output } else { scene.color }
    } else {
        let mut bytes = [0; HDR_BYTES];
        for (index, pixel) in bytes.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            let x = index as u32 % WIDTH;
            let y = index as u32 / WIDTH;
            for (word, value) in pixel
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(input_pixel(x, y, false))
            {
                *word = value.to_le_bytes();
            }
        }
        let input = graph.image("consumer HDR", input_desc()).unwrap();
        graph.upload_image("HDR pixels", input, &bytes).unwrap();
        let output = effect.add_passes(&mut graph, input).unwrap();
        if keep_effect { output } else { input }
    };
    let capture = graph.readback_image("HDR output", output).unwrap();
    (graph.compile().unwrap(), capture)
}

#[test]
fn bloom_composes_through_public_graphs_and_unused_passes_are_culled() {
    for mode in [Bloom::Off, Bloom::Tent, Bloom::Box] {
        let mut preparation = preparation();
        let effect = mode.prepare(&mut preparation, input_desc()).unwrap();
        let mut pool = GraphPool::new(preparation.graph_capacity(CAPACITY).unwrap(), 1).unwrap();
        for standard in [false, true] {
            for keep in [false, true] {
                let (graph, _) = fixture(&mut pool, &effect, standard, keep);
                let passes = graph
                    .ordered_pass_names()
                    .filter(|name| name.starts_with("Bloom "))
                    .collect::<Vec<_>>();
                if keep && mode != Bloom::Off {
                    assert_eq!(
                        passes,
                        [
                            "Bloom extract and downsample half",
                            "Bloom downsample quarter",
                            "Bloom blur",
                            "Bloom combine"
                        ]
                    );
                } else {
                    assert!(passes.is_empty());
                }
            }
        }
        assert!(pool.is_idle());
    }
}

#[test]
fn bloom_rejects_incompatible_and_uninitialized_hdr() {
    let mut preparation = preparation();
    let ldr = ImageDesc::rgba8(WIDTH, HEIGHT);
    assert!(Bloom::Tent.prepare(&mut preparation, ldr).is_err());
    let bypass = Bloom::Off.prepare(&mut preparation, ldr).unwrap();
    let effect = Bloom::Tent.prepare(&mut preparation, input_desc()).unwrap();
    let capacity = preparation
        .graph_capacity(GraphCapacity {
            resources: 7,
            ..CAPACITY
        })
        .unwrap();
    let mut pool = GraphPool::new(capacity, 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let ldr = graph.image("LDR", ldr).unwrap();
    assert_eq!(bypass.add_passes(&mut graph, ldr).unwrap(), ldr);
    assert!(effect.add_passes(&mut graph, ldr).is_err());
    let hdr = graph.image("unwritten HDR", input_desc()).unwrap();
    let output = effect.add_passes(&mut graph, hdr).unwrap();
    graph.readback_image("invalid output", output).unwrap();
    assert_eq!(
        graph.compile().unwrap_err().kind(),
        renderer::graph::GraphErrorKind::UninitializedRead
    );
    assert!(pool.is_idle());
}

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
mod native {
    use super::*;
    use renderer::{
        d3d12::{
            D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
            D3D12ValidationMode,
        },
        vulkan::{FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode, VulkanRuntime},
    };
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(30);

    fn half_value(bits: u16) -> f64 {
        let exponent = i32::from((bits >> 10) & 31);
        assert_eq!(bits >> 15, 0, "fixture channel should be nonnegative");
        assert!(exponent < 31, "non-finite HDR result");
        let fraction = f64::from(bits & 1023) / 1024.0;
        if exponent == 0 {
            fraction * 2_f64.powi(-14)
        } else {
            (1.0 + fraction) * 2_f64.powi(exponent - 15)
        }
    }

    fn expected_red(x: u32, y: u32, source: f64, standard: bool, effect: Bloom) -> f64 {
        if effect == Bloom::Off {
            return source;
        }
        if standard {
            return 19.0;
        }
        // Exact 3 x 2 quarter-size reference after reducing the two uniform
        // highlights (16 in the first cell and 8 in the last cell). These
        // constants come from the edge-extended kernel weights for those cells.
        let grid = match effect {
            Bloom::Tent => [[9.0, 3.5, 1.5], [3.0, 2.5, 4.5]],
            Bloom::Box => [[64.0, 40.0, 16.0], [32.0; 3]].map(|row| row.map(|v| v / 9.0)),
            Bloom::Off => unreachable!(),
        };
        let u = ((f64::from(x) + 0.5) / 3.0 - 0.5).clamp(0.0, 2.0);
        let v = ((f64::from(y) + 0.5) * 0.4 - 0.5).clamp(0.0, 1.0);
        let left = u.floor() as usize;
        let right = u.ceil() as usize;
        let fraction = u.fract();
        let at = |row: usize| grid[row][left] * (1.0 - fraction) + grid[row][right] * fraction;
        source + (at(0) * (1.0 - v) + at(1) * v) / 8.0
    }

    fn check_pixels(bytes: &[u8; HDR_BYTES], standard: bool, effect: Bloom) {
        for (index, pixel) in bytes.as_chunks::<8>().0.iter().enumerate() {
            let x = index as u32 % WIDTH;
            let y = index as u32 / WIDTH;
            let read =
                |channel: usize| u16::from_le_bytes([pixel[channel * 2], pixel[channel * 2 + 1]]);
            let source = input_pixel(x, y, standard);
            for (channel, expected) in source.into_iter().enumerate().skip(2) {
                assert_eq!(
                    read(channel),
                    expected,
                    "{effect:?} {x},{y} channel {channel}"
                );
            }
            if standard || effect == Bloom::Off {
                assert_eq!(read(1), source[1], "{effect:?} {x},{y} green");
            } else if effect == Bloom::Tent && [x, y] == [4, 0] {
                // The old extraction image stored both alternating tap
                // values with round-toward-zero binary16 conversion. Their
                // contribution here is 0x2359 instead of 0x235a from
                // averaging the unrounded FP32 taps.
                assert_eq!(read(1), 0x2359);
            }
            let red = read(0);
            if effect == Bloom::Off || standard {
                assert_eq!(
                    red,
                    if effect == Bloom::Off {
                        source[0]
                    } else {
                        0x4cc0
                    }
                );
            } else {
                let actual = half_value(red);
                let expected = expected_red(x, y, half_value(source[0]), standard, effect);
                // One unit of relative binary16 precision covers final storage
                // rounding and quarter-grid storage (the latter scaled by 1/8).
                assert!(
                    (actual - expected).abs() <= expected / 1024.0,
                    "{effect:?} {x},{y}: actual {actual}, expected {expected}"
                );
            }
        }
    }

    #[test]
    #[ignore = "requires same-adapter hardware Vulkan and D3D12 and requested validation layers"]
    fn consumer_bloom_gpu() {
        if cfg!(debug_assertions) {
            panic!("run the acceptance in release");
        }
        let strict = cfg!(feature = "development-validation");
        let mut vk = VulkanRuntime::new_with_validation(
            RuntimeConfig {
                execution_slots: 1,
                resource_capacity: 8,
                ..Default::default()
            },
            if strict {
                ValidationMode::Required
            } else {
                ValidationMode::Disabled
            },
        )
        .unwrap();
        let mut dx = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            capacity: D3D12FrameCapacity {
                executions: 1,
                resources: 8,
                operations: 8,
                upload_bytes: 4096,
                readback_bytes: 4096,
                ..Default::default()
            },
        })
        .unwrap();
        let adapter = vk.device_info();
        assert_ne!(adapter.device_type, "CPU");
        assert_eq!(adapter.device_luid, Some(dx.device_info().luid));
        let vk_monitor = vk.validation_monitor();
        let dx_monitor = dx.validation_monitor();
        let mut preparation = preparation();
        let off = Bloom::Off.prepare(&mut preparation, input_desc()).unwrap();
        let tent = Bloom::Tent.prepare(&mut preparation, input_desc()).unwrap();
        let mut alternative = Preparation::new(NonZeroUsize::MIN);
        let boxed = Bloom::Box.prepare(&mut alternative, input_desc()).unwrap();
        let capacity = preparation.graph_capacity(CAPACITY).unwrap();
        let working_set = preparation.working_set();
        let dx_images = working_set
            .resources
            .iter()
            .map(|class| match *class {
                ResourceClass::Image { desc, count } => D3D12ImageClass { desc, count },
                ResourceClass::Buffer { .. } => unreachable!("Bloom reserves only images"),
            })
            .collect::<Vec<_>>();
        vk.prepare_working_set(
            FrameCapacity {
                graph: capacity,
                work_slots: 1,
                ..Default::default()
            },
            working_set.resources,
        )
        .unwrap();
        let programs = working_set
            .compute
            .iter()
            .chain(alternative.working_set().compute)
            .cloned()
            .collect::<Vec<_>>();
        vk.prepare_compute_programs(&programs).unwrap();
        dx.prepare_graph_working_set(None, &dx_images, &[], &programs)
            .unwrap();

        let mut pool = GraphPool::new(capacity, 1).unwrap();
        let mut patterned_tent = None;
        for standard in [false, true] {
            for (mode, effect) in [
                (Bloom::Off, &off),
                (Bloom::Tent, &tent),
                (Bloom::Box, &boxed),
            ] {
                let (graph, capture) = fixture(&mut pool, effect, standard, true);
                let mut vk_bytes = [0; HDR_BYTES];
                let mut dx_bytes = [0; HDR_BYTES];
                let mut pending = vk.submit(&graph).unwrap();
                let mut result = pending.wait(WAIT).unwrap().expect("Vulkan completion");
                result.readback_into(capture, &mut vk_bytes).unwrap();
                drop((result, pending));
                let mut pending = dx.submit(&graph).unwrap();
                drop(graph);
                let mut result = pending.wait(WAIT).unwrap().expect("D3D12 completion");
                result.readback_into(capture, &mut dx_bytes).unwrap();
                drop((result, pending));
                check_pixels(&vk_bytes, standard, mode);
                check_pixels(&dx_bytes, standard, mode);
                assert_eq!(vk_bytes, dx_bytes, "standard={standard} {mode:?}");
                if !standard && mode == Bloom::Tent {
                    patterned_tent = Some(vk_bytes);
                }
                if !standard && mode == Bloom::Box {
                    assert_ne!(patterned_tent, Some(vk_bytes));
                }
            }
        }
        assert!(pool.is_idle());
        drop(vk);
        dx.shutdown().unwrap();
        let vk_report = vk_monitor.report();
        let dx_report = dx_monitor.snapshot();
        assert!(vk_report.is_clean(), "{vk_report:?}");
        assert!(dx_report.is_clean(), "{dx_report:?}");
        if strict {
            assert_eq!(vk_report.core_validation, Some(true));
            assert_eq!(vk_report.synchronization_validation, Some(true));
            assert!(dx_report.debug_layer_enabled && dx_report.gpu_based_validation_enabled);
            assert!(dx_report.teardown_checked);
        } else {
            assert_eq!(vk_report.requested, ValidationMode::Disabled);
            assert!(!dx_report.debug_layer_enabled && !dx_report.gpu_based_validation_enabled);
        }
        eprintln!(
            "Bloom on {}, strict={strict}: six {HDR_BYTES}-byte results per API checked",
            adapter.name
        );
    }
}

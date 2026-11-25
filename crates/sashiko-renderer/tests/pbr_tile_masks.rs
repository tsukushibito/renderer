use sashiko_renderer::pbr_forward_plus::{
    LocalLight, LocalLightLayout, active_tile_count, append_culling,
};
use sashiko_renderer_core::graph::{
    BufferDesc, CompiledGraph, GraphCapacity, GraphPool, PassHandle,
};

const CAMERA: BufferDesc = BufferDesc { size: 64 };
const CAPACITY: usize = 129;
const WORDS: usize = CAPACITY.div_ceil(32);
fn layout() -> LocalLightLayout {
    LocalLightLayout::new(CAPACITY).unwrap()
}

fn light(position: [f32; 3], radius: f32) -> LocalLight {
    LocalLight::Point(
        sashiko_renderer::pbr::PointLight::from_candela(position, radius, [1.0, 0.5, 0.25], 2.0)
            .unwrap(),
    )
}

fn fixture(
    pool: &mut GraphPool,
    width: u32,
    height: u32,
    lights: &[LocalLight],
) -> (CompiledGraph, PassHandle) {
    let mut graph = pool.begin().unwrap();
    let camera = graph.buffer("fixture view", CAMERA).unwrap();

    let output = graph
        .image(
            "fixture tile masks",
            layout().tile_mask_desc(width, height).unwrap(),
        )
        .unwrap();
    let mut camera_bytes = [0; 64];
    for word in [0, 5, 10, 15] {
        let offset = word * 4;
        camera_bytes[offset..offset + 4].copy_from_slice(&1.0_f32.to_le_bytes());
    }
    graph
        .upload_buffer("view", camera, 0, &camera_bytes)
        .unwrap();
    let mut bytes = vec![0; layout().byte_len()];
    layout()
        .encode([width, height], lights, &mut bytes)
        .unwrap();
    let input = layout().upload(&mut graph, &bytes).unwrap();
    let view = graph.image_view(output, 0, 0).unwrap();
    append_culling(&mut graph, camera, input, view).unwrap();
    let capture = graph.readback_image("tile capture", output).unwrap();
    (graph.compile().unwrap(), capture)
}

#[test]
fn variable_extents_size_and_initialize_the_whole_view_without_imports() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for (width, height, shape) in [
        (16, 16, [5, 1]),
        (17, 17, [10, 2]),
        (33, 29, [15, 2]),
        (113, 79, [40, 5]),
    ] {
        let desc = layout().tile_mask_desc(width, height).unwrap();
        assert_eq!([desc.width, desc.height], shape);
        assert_eq!(
            active_tile_count(width, height).unwrap(),
            desc.width * desc.height / WORDS as u32
        );
        let (graph, _) = fixture(&mut pool, width, height, &[]);
        assert_eq!(
            graph.ordered_pass_names().collect::<Vec<_>>(),
            [
                "view",
                "local light update",
                "Forward+ tile light masks",
                "tile capture"
            ]
        );
    }
    assert!(pool.is_idle());
    assert!(layout().tile_mask_desc(u32::MAX, u32::MAX).is_err());
    assert!(layout().tile_mask_desc(0, 16).is_err());
    assert!(
        layout()
            .encode(
                [32, 32],
                &[light([0.0; 3], 1.0); CAPACITY + 1],
                &mut vec![0; layout().byte_len()]
            )
            .is_err()
    );
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
mod native {
    use super::*;
    use sashiko_renderer::pbr_forward_plus::{TILE_SIZE, compute_program};
    use sashiko_renderer_core::{
        capacity::{ResourceClass, WorkingSet},
        runtime::{Runtime, RuntimeConfig, RuntimeLimits},
    };
    use std::time::Duration;

    fn exact_half_integer(value: u8) -> u16 {
        let value = u16::from(value);
        if value == 0 {
            return 0;
        }
        let exponent = value.ilog2();
        ((exponent as u16 + 15) << 10) | ((value << (10 - exponent)) & 1023)
    }

    fn check_tiles(width: u32, height: u32, lights: &[LocalLight], bytes: &[u8]) {
        let columns = width.div_ceil(TILE_SIZE);
        let to_clip = |px: u32, extent: u32| 2.0 * f64::from(px) / f64::from(extent) - 1.0;
        for (tile, encoded) in bytes.as_chunks::<{ WORDS * 8 }>().0.iter().enumerate() {
            let first_x = (tile as u32 % columns) * TILE_SIZE;
            let first_y = (tile as u32 / columns) * TILE_SIZE;
            let bounds = [
                [
                    to_clip(first_x, width),
                    to_clip((first_x + TILE_SIZE).min(width), width),
                ],
                [
                    to_clip(first_y, height),
                    to_clip((first_y + TILE_SIZE).min(height), height),
                ],
                [0.0, 1.0],
            ];
            // Under identity projection the six planes are three intervals.
            let mut masks = [0u32; WORDS];
            for (id, light) in lights.iter().enumerate() {
                let LocalLight::Point(point) = *light else {
                    unreachable!()
                };
                let position = point.position();
                let radius = f64::from(point.range());
                if position
                    .into_iter()
                    .zip(bounds)
                    .all(|(p, [lo, hi])| f64::from(p) + radius > lo && f64::from(p) - radius < hi)
                {
                    masks[id / 32] |= 1u32 << (id % 32);
                }
            }
            for (channel, (expected, actual)) in masks
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .zip(encoded.as_chunks::<2>().0)
                .enumerate()
            {
                assert_eq!(
                    u16::from_le_bytes(*actual),
                    exact_half_integer(expected),
                    "tile {tile} channel {channel}"
                );
            }
        }
    }

    fn check_gpu(config: RuntimeConfig) {
        let graph_capacity = GraphCapacity::default();
        let mut runtime = Runtime::new(
            config,
            RuntimeLimits {
                graph: graph_capacity,
                frames: 3,
                results: 3,
                ..RuntimeLimits::default()
            },
            WorkingSet {
                resources: &[
                    ResourceClass::Buffer {
                        desc: CAMERA,
                        count: 3,
                    },
                    ResourceClass::Image {
                        desc: layout().image_desc(),
                        count: 3,
                    },
                    ResourceClass::Image {
                        desc: layout().tile_mask_desc(33, 29).unwrap(),
                        count: 3,
                    },
                    ResourceClass::Image {
                        desc: layout().tile_mask_desc(113, 79).unwrap(),
                        count: 1,
                    },
                ],
                compute: &[compute_program()],
                ..WorkingSet::default()
            },
        )
        .unwrap();
        #[cfg(feature = "vulkan")]
        if let Some(vulkan) = runtime.as_vulkan() {
            assert_ne!(vulkan.device_info().device_type, "CPU");
        }
        let mixed = [
            light([-0.62, -0.54, 0.5], 0.10),
            light([0.66, 0.70, 0.55], 0.12),
            light([32.0 / 33.0 - 1.0, 0.10, 0.35], 0.23), // Straddles a tile boundary.
            light([0.67, 0.70, -0.3], 0.12),              // Entirely behind the near clip plane.
            light([0.98, 0.25, 0.55], 0.12),              // Partially covered rightmost tile.
        ];
        let saturated = [light([0.0, 0.0, 0.5], 2.0); CAPACITY];
        let mut odd_lights = [light([0.67, 0.70, -0.3], 0.12); CAPACITY];
        odd_lights[33] = mixed[0];
        odd_lights[128] = mixed[4];
        let cases = [
            [
                (33, 29, &[][..]),
                (33, 29, &saturated[..]),
                (33, 29, &[][..]),
            ],
            [
                (33, 29, &mixed[..]),
                (113, 79, &odd_lights[..]),
                (33, 29, &saturated[..]),
            ],
        ];
        let mut pool = GraphPool::new(graph_capacity, 3).unwrap();
        let timeout = Duration::from_secs(30);
        for batch in cases {
            // All three masks are live before any result is consumed.
            let captures = batch.map(|(width, height, lights)| {
                let (graph, capture) = fixture(&mut pool, width, height, lights);
                (runtime.submit(&graph).unwrap().id(), capture)
            });
            for ((frame, capture), (width, height, lights)) in captures.into_iter().zip(batch) {
                let bytes_len = active_tile_count(width, height).unwrap() as usize * WORDS * 8;
                let mut bytes = vec![0xff; bytes_len];
                let mut result = runtime.wait_for_frame(frame, timeout).unwrap().unwrap();
                result.readback_into(capture, &mut bytes).unwrap();
                check_tiles(width, height, lights, &bytes);
            }
            assert!(pool.is_idle());
        }
        let strict = cfg!(feature = "development-validation");
        match config {
            #[cfg(feature = "vulkan")]
            RuntimeConfig::Vulkan(_) => {
                let monitor = runtime.as_vulkan().unwrap().validation_monitor();
                drop(runtime);
                let report = monitor.report();
                assert!(report.is_clean(), "{report:#?}");
                if strict {
                    assert_eq!(report.core_validation, Some(true));
                    assert_eq!(report.synchronization_validation, Some(true));
                }
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeConfig::D3D12(_) => {
                let monitor = runtime.as_d3d12().unwrap().validation_monitor();
                drop(runtime);
                let report = monitor.snapshot();
                assert!(report.is_clean(), "{report:#?}");
                assert_eq!(report.debug_layer_enabled, strict);
                assert_eq!(report.gpu_based_validation_enabled, strict);
            }
        }
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a real Vulkan device"]
    fn vulkan_tile_masks_match_analytic_reference() {
        check_gpu(RuntimeConfig::Vulkan(Default::default()));
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a real D3D12 device"]
    fn d3d12_tile_masks_match_analytic_reference() {
        check_gpu(RuntimeConfig::D3D12(Default::default()));
    }
}

//! Consumer-owned light lists with an independent orthographic reference.

#[path = "../examples/support/bistro/program/forward_plus/culling.rs"]
mod culling;

use culling::{
    LIGHT_BUFFER_BYTES, MAX_LIGHTS, PointLight, TILE_BYTES, TILES_PER_GROUP, active_tile_count,
    append_culling, light_bytes, workgroups,
};
use renderer::graph::{BufferDesc, CompiledGraph, GraphCapacity, GraphPool, PassHandle};

const OUTPUT_BYTES: u64 = TILE_BYTES * (2 * TILES_PER_GROUP) as u64;
const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn light(position: [f32; 3], radius: f32) -> PointLight {
    PointLight {
        position,
        radius,
        color: [1.0, 0.5, 0.25],
        intensity: 2.0,
    }
}

fn fixture(
    pool: &mut GraphPool,
    width: u32,
    height: u32,
    lights: &[PointLight],
) -> (CompiledGraph, PassHandle, usize) {
    let mut graph = pool.begin().unwrap();
    let camera = graph
        .buffer("fixture view", BufferDesc { size: 64 })
        .unwrap();
    let input = graph
        .buffer(
            "fixture lights",
            BufferDesc {
                size: LIGHT_BUFFER_BYTES,
            },
        )
        .unwrap();
    let output = graph
        .buffer("fixture tile lists", BufferDesc { size: OUTPUT_BYTES })
        .unwrap();
    let mut camera_bytes = [0; 64];
    for (destination, word) in camera_bytes.as_chunks_mut::<4>().0.iter_mut().zip(IDENTITY) {
        *destination = word.to_le_bytes();
    }
    graph
        .upload_buffer("view", camera, 0, &camera_bytes)
        .unwrap();
    graph
        .upload_buffer(
            "lights",
            input,
            0,
            &light_bytes(width, height, lights).unwrap(),
        )
        .unwrap();
    append_culling(&mut graph, width, height, camera, input, output).unwrap();
    let groups = u64::from(workgroups(width, height).unwrap()[0]);
    let written = groups * u64::from(TILES_PER_GROUP) * TILE_BYTES;
    let capture = graph
        .readback_buffer("tile capture", output, 0, written)
        .unwrap();
    (graph.compile().unwrap(), capture, written as usize)
}

#[test]
fn variable_view_extents_use_the_same_culling_program_and_reserve_complete_groups() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for (width, height, tiles) in [(16, 16, 1u32), (17, 17, 4), (33, 29, 6), (113, 79, 40)] {
        let (graph, _, bytes) = fixture(&mut pool, width, height, &[]);
        assert_eq!(active_tile_count(width, height).unwrap(), tiles);
        let padded = tiles.div_ceil(TILES_PER_GROUP) * TILES_PER_GROUP;
        assert_eq!(bytes, (u64::from(padded) * TILE_BYTES) as usize);
        assert_eq!(
            graph.ordered_pass_names().collect::<Vec<_>>(),
            [
                "view",
                "lights",
                "Forward+ tile light lists",
                "tile capture"
            ]
        );
    }
    assert!(pool.is_idle());
    assert!(workgroups(u32::MAX, u32::MAX).is_err());
    assert!(workgroups(0, 16).is_err());
    assert!(light_bytes(32, 32, &[light([0.0; 3], 1.0); MAX_LIGHTS + 1]).is_err());
}

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
mod native {
    use super::*;
    use culling::{TILE_SIZE, compute_program};
    use renderer::{
        D3D12BufferClass, D3D12FrameCapacity, D3D12Runtime, D3D12RuntimeConfig,
        D3D12ValidationMode, FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode,
        VulkanRuntime,
    };
    use std::time::Duration;

    fn check_tiles(width: u32, height: u32, lights: &[PointLight], bytes: &[u8]) {
        let columns = width.div_ceil(TILE_SIZE);
        let to_clip = |px: u32, extent: u32| 2.0 * f64::from(px) / f64::from(extent) - 1.0;
        for (tile, words) in bytes
            .as_chunks::<{ TILE_BYTES as usize }>()
            .0
            .iter()
            .map(|record| {
                record
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .copied()
                    .map(u32::from_le_bytes)
            })
            .enumerate()
        {
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
            // With identity projection the conservative six sides are just
            // three scalar intervals. No camera-plane math is needed here.
            let expected: Vec<u32> = lights
                .iter()
                .enumerate()
                .filter(|(_, light)| {
                    let radius = f64::from(light.radius);
                    light.position.into_iter().zip(bounds).all(|(p, [lo, hi])| {
                        f64::from(p) + radius > lo && f64::from(p) - radius < hi
                    })
                })
                .map(|(id, _)| id as u32)
                .collect();
            let actual: Vec<_> = words.collect();
            assert_eq!(actual[0] as usize, expected.len(), "tile {tile}");
            assert_eq!(&actual[1..1 + expected.len()], expected, "tile {tile}");
            assert!(
                actual[1 + expected.len()..].iter().all(|word| *word == 0),
                "tile {tile} tail"
            );
        }
    }

    #[test]
    #[ignore = "requires Vulkan and D3D12 on the same hardware adapter"]
    fn tile_lists_match_analytic_reference() {
        let strict = cfg!(feature = "development-validation");
        let mut vk = VulkanRuntime::new_with_validation(
            RuntimeConfig::default(),
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
                resources: 4,
                resource_capacity: 4,
                buffers: 3,
                operations: 8,
                upload_bytes: 4096,
                readback_bytes: OUTPUT_BYTES,
                ..Default::default()
            },
        })
        .unwrap();
        let adapter = vk.device_info();
        assert_ne!(adapter.device_type, "CPU");
        assert_eq!(adapter.device_luid, Some(dx.device_info().luid));
        let vk_monitor = vk.validation_monitor();
        let dx_monitor = dx.validation_monitor();
        let buffer_sizes = [64, LIGHT_BUFFER_BYTES, OUTPUT_BYTES];
        vk.prepare_working_set(
            FrameCapacity {
                work_slots: 1,
                ..Default::default()
            },
            &buffer_sizes.map(|size| ResourceClass::Buffer {
                desc: BufferDesc { size },
                count: 1,
            }),
        )
        .unwrap();
        let programs = [compute_program()];
        vk.prepare_compute_programs(&programs).unwrap();
        dx.prepare_graph_working_set(
            Some(&buffer_sizes.map(|size| D3D12BufferClass { size, count: 1 })),
            &[],
            &[],
            &programs,
        )
        .unwrap();

        let mixed = [
            light([-0.62, -0.54, 0.5], 0.10),
            light([0.66, 0.70, 0.55], 0.12),
            // This sphere straddles the boundary at x = 16 between active tiles.
            light([32.0 / 33.0 - 1.0, 0.10, 0.35], 0.23),
            light([0.67, 0.70, -0.3], 0.12), // Entirely behind the near clip plane.
            light([0.98, 0.25, 0.55], 0.12), // Partially covered rightmost tile.
        ];
        let saturated = [light([0.0, 0.0, 0.5], 2.0); MAX_LIGHTS];
        let cases = [
            (33, 29, mixed.as_slice()),
            (113, 79, &[]),
            (16, 16, &saturated),
        ];
        let wait = Duration::from_secs(30);
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        for (width, height, lights) in cases {
            let (graph, capture, byte_count) = fixture(&mut pool, width, height, lights);
            let mut vk_bytes = vec![0; byte_count];
            let mut dx_bytes = vec![0; byte_count];
            {
                let mut pending = vk.submit(&graph).unwrap();
                let mut result = pending.wait(wait).unwrap().expect("Vulkan completion");
                result.readback_into(capture, &mut vk_bytes).unwrap();
            }
            {
                let mut pending = dx.submit(&graph).unwrap();
                let mut result = pending.wait(wait).unwrap().expect("D3D12 completion");
                result.readback_into(capture, &mut dx_bytes).unwrap();
            }
            let active =
                (u64::from(active_tile_count(width, height).unwrap()) * TILE_BYTES) as usize;
            check_tiles(width, height, lights, &vk_bytes[..active]);
            assert!(vk_bytes[active..].iter().all(|byte| *byte == 0));
            assert_eq!(vk_bytes, dx_bytes, "{width}×{height} Forward+ tile lists");
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
        }
    }
}

pub use sashiko_renderer::passes::visibility::{
    VISIBILITY_AABB_BYTES, VISIBILITY_CAMERA_BYTES, VISIBILITY_MASK_BYTES, VISIBILITY_THREADS,
    visibility_program,
};

#[cfg(all(test, feature = "vulkan", feature = "d3d12", windows))]
mod native {
    use super::*;
    use sashiko_renderer_core::graph::{
        Binding, BindingResource, BufferDesc, CompiledGraph, GraphPool, PassHandle,
    };
    use sashiko_renderer_core::{
        d3d12::{
            D3D12BufferClass, D3D12FrameCapacity, D3D12Runtime, D3D12RuntimeConfig,
            D3D12ValidationMode,
        },
        vulkan::{FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode, VulkanRuntime},
    };
    use std::time::Duration;

    const SLOTS: usize = 2 * VISIBILITY_THREADS as usize;
    const MASK_BYTES: usize = SLOTS * VISIBILITY_MASK_BYTES;
    const BOUNDS_BYTES: usize = SLOTS * VISIBILITY_AABB_BYTES;
    const BUFFER_SIZES: [u64; 3] = [
        VISIBILITY_CAMERA_BYTES as u64,
        BOUNDS_BYTES as u64,
        MASK_BYTES as u64,
    ];

    fn fixture(
        pool: &mut GraphPool,
        groups: u32,
        uncertain: bool,
    ) -> (CompiledGraph, PassHandle, Vec<u32>) {
        let mut graph = pool.begin().unwrap();
        let [camera, bounds, mask] = BUFFER_SIZES.map(|size| {
            graph
                .buffer("visibility fixture", BufferDesc { size })
                .unwrap()
        });
        let [x, y, z, w] = [
            [if uncertain { f32::MAX } else { 1.0 }, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let camera_bytes: Vec<_> = [x, y, z, w]
            .into_iter()
            .flatten()
            .flat_map(f32::to_le_bytes)
            .collect();
        let origin = [0.0, 0.0, 0.5];
        let cases = [
            (origin, origin, 1, true),
            ([2.0, 0.0, 0.5], [2.0, 0.0, 0.5], 1, false),
            ([-2.0, 0.0, 0.5], [-2.0, 0.0, 0.5], 1, false),
            ([0.0, 2.0, 0.5], [0.0, 2.0, 0.5], 1, false),
            ([0.0, -2.0, 0.5], [0.0, -2.0, 0.5], 1, false),
            ([0.0, 0.0, -0.5], [0.0, 0.0, -0.5], 1, false),
            ([0.0, 0.0, 1.5], [0.0, 0.0, 1.5], 1, false),
            ([0.75, 0.0, 0.5], [1.5, 0.0, 0.5], 1, true),
            ([1.0, 0.0, 0.0], [1.0, 0.0, 1.0], 1, true),
            (
                [1.0 + f32::EPSILON, 0.0, 0.5],
                [1.0 + f32::EPSILON, 0.0, 0.5],
                1,
                true,
            ),
            ([f32::NAN, 0.0, 0.5], [2.0, 0.0, 0.5], 1, true),
            ([2.0, 0.0, 0.5], [1.0, 0.0, 0.5], 1, true),
            (origin, origin, 0, false),
        ];
        let slots = groups as usize * VISIBILITY_THREADS as usize;
        let mut expected = vec![0; slots];
        let mut bounds_bytes = vec![0; BOUNDS_BYTES];
        for group in 0..groups as usize {
            for (index, (minimum, maximum, enabled, visible)) in cases.into_iter().enumerate() {
                let slot = group * VISIBILITY_THREADS as usize + index;
                expected[slot] = u32::from(enabled != 0 && (visible || uncertain));
                let first = slot * VISIBILITY_AABB_BYTES;
                let words = [
                    minimum[0].to_bits(),
                    minimum[1].to_bits(),
                    minimum[2].to_bits(),
                    enabled,
                    maximum[0].to_bits(),
                    maximum[1].to_bits(),
                    maximum[2].to_bits(),
                    0,
                ];
                bounds_bytes[first..first + VISIBILITY_AABB_BYTES]
                    .copy_from_slice(words.map(u32::to_le_bytes).as_flattened());
            }
        }
        graph
            .upload_buffer("camera", camera, 0, &camera_bytes)
            .unwrap();
        graph
            .upload_buffer("bounds", bounds, 0, &bounds_bytes)
            .unwrap();
        let mask_bytes = slots * VISIBILITY_MASK_BYTES;
        let bindings = [
            (camera, VISIBILITY_CAMERA_BYTES),
            (bounds, slots * VISIBILITY_AABB_BYTES),
            (mask, mask_bytes),
        ]
        .into_iter()
        .enumerate()
        .map(|(binding, (buffer, size))| Binding {
            group: 0,
            binding: binding as u32,
            resource: BindingResource::Buffer {
                buffer,
                offset: 0,
                size: size as u64,
            },
        })
        .collect::<Vec<_>>();
        graph
            .dispatch_buffer_grid(
                "frustum visibility",
                &visibility_program(),
                &bindings,
                [groups, 1],
            )
            .unwrap();
        let capture = graph
            .readback_buffer("visibility masks", mask, 0, mask_bytes as u64)
            .unwrap();
        (graph.compile().unwrap(), capture, expected)
    }

    #[test]
    #[ignore = "requires Vulkan and Direct3D 12 hardware"]
    fn frustum_visibility_masks_cover_clip_planes_padding_and_uncertain_arithmetic() {
        if cfg!(debug_assertions) {
            panic!("run native acceptance in release");
        }
        let strict = cfg!(feature = "development-validation");
        let mut vk = VulkanRuntime::new_with_validation(
            RuntimeConfig {
                execution_slots: 1,
                resource_capacity: 4,
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
                resources: 4,
                operations: 4,
                upload_bytes: 8192,
                readback_bytes: 4096,
                ..Default::default()
            },
        })
        .unwrap();
        let vk_monitor = vk.validation_monitor();
        let dx_monitor = dx.validation_monitor();
        assert_ne!(vk.device_info().device_type, "CPU");
        assert_eq!(vk.device_info().device_luid, Some(dx.device_info().luid));
        vk.prepare_working_set(
            FrameCapacity {
                work_slots: 1,
                ..Default::default()
            },
            &BUFFER_SIZES.map(|size| ResourceClass::Buffer {
                desc: BufferDesc { size },
                count: 1,
            }),
        )
        .unwrap();
        let programs = [visibility_program()];
        vk.prepare_compute_programs(&programs).unwrap();
        dx.prepare_graph_working_set(
            Some(&BUFFER_SIZES.map(|size| D3D12BufferClass { size, count: 1 })),
            &[],
            &[],
            &programs,
        )
        .unwrap();
        let mut pool = GraphPool::new(Default::default(), 1).unwrap();
        let wait = Duration::from_secs(30);
        for (groups, uncertain) in [(1, false), (2, false), (2, true)] {
            let (plan, capture, expected) = fixture(&mut pool, groups, uncertain);
            let expected: Vec<_> = expected.into_iter().flat_map(u32::to_le_bytes).collect();
            let mut bytes = vec![0; expected.len()];
            let mut submission = vk.submit(&plan).unwrap();
            let mut done = submission
                .wait(wait)
                .unwrap()
                .expect("Vulkan visibility completion");
            done.readback_into(capture, &mut bytes).unwrap();
            assert_eq!(
                bytes, expected,
                "Vulkan visibility groups={groups} uncertain={uncertain}"
            );
            drop((done, submission));
            let mut submission = dx.submit(&plan).unwrap();
            let mut done = submission
                .wait(wait)
                .unwrap()
                .expect("D3D12 visibility completion");
            done.readback_into(capture, &mut bytes).unwrap();
            assert_eq!(
                bytes, expected,
                "D3D12 visibility groups={groups} uncertain={uncertain}"
            );
            drop((done, submission));
        }
        drop(vk);
        dx.shutdown().unwrap();
        let vk_report = vk_monitor.report();
        let dx_report = dx_monitor.snapshot();
        assert!(vk_report.is_clean(), "{vk_report:?}");
        assert!(dx_report.is_clean(), "{dx_report:?}");
    }
}

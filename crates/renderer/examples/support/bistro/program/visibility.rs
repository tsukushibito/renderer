use super::packaged_shaders;
use renderer::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, ShaderStages,
};

pub const VISIBILITY_CAMERA_BYTES: usize = 64;
pub const VISIBILITY_THREADS: u32 = 64;
pub const VISIBILITY_AABB_BYTES: usize = 32;
pub const VISIBILITY_MASK_BYTES: usize = 4;

const BOUNDS_GROUP_BYTES: u64 = VISIBILITY_THREADS as u64 * VISIBILITY_AABB_BYTES as u64;
const MASK_GROUP_BYTES: u64 = VISIBILITY_THREADS as u64 * VISIBILITY_MASK_BYTES as u64;

static VISIBILITY: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [VISIBILITY_THREADS, 1, 1],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::Uniform,
            min_size: VISIBILITY_CAMERA_BYTES as u64,
            stages: ShaderStages::COMPUTE,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::StorageRead,
            min_size: BOUNDS_GROUP_BYTES,
            stages: ShaderStages::COMPUTE,
        },
        BindingLayout {
            group: 0,
            binding: 2,
            kind: BindingType::StorageWrite,
            min_size: MASK_GROUP_BYTES,
            stages: ShaderStages::COMPUTE,
        },
    ],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::BufferGrid2d {
            group: 0,
            binding: 2,
            max_workgroups: [65_535, 1],
            read_bindings: &[(0, 1)],
        },
        bytes: &[
            VISIBILITY_CAMERA_BYTES as u64,
            BOUNDS_GROUP_BYTES,
            MASK_GROUP_BYTES,
        ],
    },
    shaders: packaged_shaders!(
        include_bytes!("../../../../../renderer-pipeline/shaders/frustum_visibility.comp.spv"),
        include_bytes!(
            "../../../../../renderer-pipeline/shaders/dxil/frustum_visibility.comp.dxil"
        ),
    ),
};

pub const fn visibility_program() -> ManagedProgram {
    // SAFETY: Both assets match the exact column-major camera prefix and
    // padded record layout. On a bounded 1-D dispatch, each thread reads at
    // most its own 32-byte bounds record and writes its own 4-byte mask.
    // Even disabled records initialize the mask. Values never select an
    // address, and the accepted extents fit the D3D12 raw byte offsets.
    unsafe { ManagedProgram::from_trusted_descriptor(&VISIBILITY) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frustum_visibility_reflection_matches_packed_shader_inputs_and_output() {
        let spirv: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../renderer-pipeline/shaders/dxil/frustum_visibility.comp.spirv-reflection.json"
        ))
        .unwrap();
        let entry = &spirv["entryPoints"][0];
        assert_eq!(entry["name"], "main");
        assert_eq!(
            entry["workgroup_size"],
            serde_json::json!([VISIBILITY_THREADS, 1, 1])
        );

        let uniforms = spirv["ubos"].as_array().unwrap();
        assert_eq!(uniforms.len(), 1);
        let camera = &uniforms[0];
        assert_eq!(camera["set"], 0);
        assert_eq!(camera["binding"], 0);
        assert_eq!(camera["block_size"], VISIBILITY_CAMERA_BYTES);
        let matrix = &spirv["types"][camera["type"].as_str().unwrap()]["members"][0];
        assert_eq!(matrix["type"], "mat4");
        assert_eq!(matrix["offset"], 0);
        assert_eq!(matrix["matrix_stride"], 16);
        assert!(matrix.get("row_major").is_none());

        let storage = spirv["ssbos"].as_array().unwrap();
        assert_eq!(storage.len(), 2);
        for (slot, stride, qualifier) in [
            (1, VISIBILITY_AABB_BYTES, "readonly"),
            (2, VISIBILITY_MASK_BYTES, "writeonly"),
        ] {
            let buffer = storage
                .iter()
                .find(|buffer| buffer["binding"] == slot)
                .unwrap();
            assert_eq!(buffer["set"], 0);
            assert_eq!(buffer[qualifier], true);
            let member = &spirv["types"][buffer["type"].as_str().unwrap()]["members"][0];
            assert_eq!(member["offset"], 0);
            assert_eq!(member["array_stride"], stride);
            assert_eq!(member["array"], serde_json::json!([0]));
            if slot == 1 {
                let fields = spirv["types"][member["type"].as_str().unwrap()]["members"]
                    .as_array()
                    .unwrap();
                assert_eq!(fields.len(), 2);
                for (field, offset) in fields.iter().zip([0, 16]) {
                    assert_eq!(field["type"], "uvec4");
                    assert_eq!(field["offset"], offset);
                }
            } else {
                assert_eq!(member["type"], "uint");
            }
        }

        let dxil = include_str!(
            "../../../../../renderer-pipeline/shaders/dxil/frustum_visibility.comp.reflection.txt"
        );
        assert!(dxil.contains("; NumThreads=(64,1,1)"));
        assert!(
            dxil.lines().any(|line| line.ends_with("Size:    64")),
            "DXIL camera prefix differs"
        );
        let bindings: Vec<_> = dxil
            .split_once("; Resource Bindings:")
            .unwrap()
            .1
            .lines()
            .filter(|line| line.trim_end().ends_with(" 1"))
            .map(|line| line.split_whitespace().rev().nth(1).unwrap())
            .collect();
        assert_eq!(bindings, ["cb0", "t1", "u2"]);
    }
}

#[cfg(all(test, feature = "vulkan", feature = "d3d12", windows))]
mod native {
    use super::*;
    use renderer::graph::{
        Binding, BindingResource, BufferDesc, CompiledGraph, GraphPool, PassHandle,
    };
    use renderer::{
        D3D12BufferClass, D3D12FrameCapacity, D3D12Runtime, D3D12RuntimeConfig,
        D3D12ValidationMode, FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode,
        VulkanRuntime,
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

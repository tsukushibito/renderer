//! Consumer-owned Compute programs: public Graph contracts and explicit two-API acceptance.

#[path = "../examples/support/compute_fixture.rs"]
mod fixture;

use renderer::graph::{
    Binding, BindingResource, BufferDesc, GraphCapacity, GraphErrorKind, GraphPool,
};

#[test]
fn consumer_owned_hdr_programs_form_a_changing_multistage_graph() {
    let _programs = fixture::programs();
    let mut pool = GraphPool::new(fixture::capacity(), 1).unwrap();
    for frame in 0..12 {
        let (graph, expected) = fixture::build(&mut pool, frame);
        let plan = graph.compile().unwrap();
        assert_eq!(expected.readbacks.len(), 4);
        assert_ne!(expected.readbacks[0].pass, expected.readbacks[1].pass);
        assert_ne!(expected.readbacks[0].bytes, expected.readbacks[1].bytes);
        assert!(
            expected
                .readbacks
                .iter()
                .all(|entry| !entry.name.is_empty())
        );
        assert_eq!(
            expected
                .readbacks
                .iter()
                .map(|entry| entry.len)
                .sum::<usize>(),
            fixture::READBACK_BYTES
        );
        let names = plan.ordered_pass_names().collect::<Vec<_>>();
        assert!(names.contains(&"sample HDR with per-frame uniform"));
        assert_eq!(
            names
                .iter()
                .filter(|&&name| name == "storage HDR with GPU-produced offset")
                .count(),
            frame % 3 + 1,
        );
        assert!(names.contains(&"copy final HDR"));
        assert!(names.contains(&"copy GPU buffer result"));
        drop(plan);
        assert!(pool.is_idle());
    }
}

fn storage_inputs(
    graph: &mut renderer::graph::Graph,
    initialize_source: bool,
    feedback: bool,
    buffer_size: u64,
) -> [Binding; 3] {
    let source = graph.image("source", fixture::hdr()).unwrap();
    if initialize_source {
        graph
            .upload_image("source bytes", source, &[0; fixture::IMAGE_BYTES])
            .unwrap();
    }
    let destination = if feedback {
        source
    } else {
        graph.image("destination", fixture::hdr()).unwrap()
    };
    let offsets = graph.buffer("offsets", BufferDesc { size: 16 }).unwrap();
    graph
        .upload_buffer("offset bytes", offsets, 0, &[0; 16])
        .unwrap();
    [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::ImageView(graph.image_view(source, 0, 0).unwrap()),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::Buffer {
                buffer: offsets,
                offset: 0,
                size: buffer_size,
            },
        },
        Binding {
            group: 1,
            binding: 0,
            resource: BindingResource::ImageView(graph.image_view(destination, 0, 0).unwrap()),
        },
    ]
}

#[test]
fn storage_image_read_requires_initialized_contents() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let inputs = storage_inputs(&mut graph, false, false, 16);
    let pass = graph
        .dispatch(
            "uninitialized image",
            &fixture::storage_program(),
            &inputs,
            [1, 2, 1],
        )
        .unwrap();
    let BindingResource::ImageView(output) = inputs[2].resource else {
        unreachable!()
    };
    graph
        .readback_image("observe storage output", output.image())
        .unwrap();
    let error = graph.compile().unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
    assert_eq!(error.pass(), Some(pass));
}

#[test]
fn overlapping_storage_images_and_short_buffer_abi_are_rejected() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for (feedback, bytes) in [(true, 16), (false, 8)] {
        let mut graph = pool.begin().unwrap();
        let inputs = storage_inputs(&mut graph, true, feedback, bytes);
        let error = graph
            .dispatch(
                "invalid binding",
                &fixture::storage_program(),
                &inputs,
                [1, 2, 1],
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        drop(graph);
        assert!(pool.is_idle());
    }
}

#[test]
fn invalid_image_domain_rolls_back_the_complete_dispatch() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            bindings: 3,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    for failure in 0..3 {
        let mut graph = pool.begin().unwrap();
        let valid = storage_inputs(&mut graph, true, false, 16);
        let mut invalid = valid;
        let groups = if failure == 0 { [1; 3] } else { [1, 2, 1] };
        if failure != 0 {
            let desc = if failure == 1 {
                renderer::graph::ImageDesc::rgba8(fixture::WIDTH, fixture::HEIGHT)
            } else {
                renderer::graph::ImageDesc {
                    height: fixture::HEIGHT + 1,
                    ..fixture::hdr()
                }
            };
            let incompatible = graph.image("incompatible image", desc).unwrap();
            invalid[2].resource =
                BindingResource::ImageView(graph.image_view(incompatible, 0, 0).unwrap());
        }
        let error = graph
            .dispatch(
                "invalid image domain",
                &fixture::storage_program(),
                &invalid,
                groups,
            )
            .unwrap_err();
        assert_eq!(
            error.kind(),
            if failure == 0 {
                GraphErrorKind::Unsupported
            } else {
                GraphErrorKind::InvalidInput
            }
        );
        graph
            .dispatch(
                "valid retry",
                &fixture::storage_program(),
                &valid,
                [1, 2, 1],
            )
            .unwrap();
        let BindingResource::ImageView(output) = valid[2].resource else {
            unreachable!()
        };
        graph
            .readback_image("valid result", output.image())
            .unwrap();
        let plan = graph.compile().unwrap();
        assert!(
            !plan
                .ordered_pass_names()
                .any(|name| name == "invalid image domain")
        );
        drop(plan);
        assert!(pool.is_idle());
    }
}

#[cfg(all(
    windows,
    feature = "vulkan",
    feature = "d3d12",
    any(
        all(feature = "d3d12-diagnostics", feature = "vulkan-diagnostics"),
        not(any(
            feature = "development-validation",
            feature = "d3d12-diagnostics",
            feature = "vulkan-diagnostics",
            feature = "graph-diagnostics"
        ))
    )
))]
mod native {
    use super::fixture;
    use renderer::graph::{
        Attachment, Binding, BindingResource, BufferDesc, BufferHandle, CompiledGraph,
        GraphCapacity, GraphPool, ImageDesc, LoadOp, PassHandle, RasterAttachments, StoreOp,
    };
    use renderer::program::{ManagedProgram, SamplerDescriptor};
    use renderer::{
        D3D12BufferClass, D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
        D3D12RuntimeErrorKind, D3D12ValidationMode, FrameCapacity, ImageFormat, ResourceClass,
        RuntimeConfig, RuntimeErrorKind, ValidationMode, VulkanRuntime,
    };
    use serde_json::{Value, json};
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(30);

    fn plan() -> (GraphPool, CompiledGraph, fixture::Expected) {
        let mut pool = GraphPool::new(fixture::capacity(), 1).unwrap();
        let (graph, expected) = fixture::build(&mut pool, 2);
        (pool, graph.compile().unwrap(), expected)
    }

    fn unprepared_buffer_plan() -> (GraphPool, CompiledGraph) {
        let mut pool = GraphPool::new(fixture::capacity(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let uniform = graph.buffer("uniform", BufferDesc { size: 512 }).unwrap();
        let output = graph.buffer("output", BufferDesc { size: 64 }).unwrap();
        graph
            .upload_buffer("uniform bytes", uniform, 0, &[0; 16])
            .unwrap();
        graph
            .dispatch(
                "unprepared buffer Compute",
                &renderer::program::ManagedProgram::uniform_readback(),
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: uniform,
                            offset: 0,
                            size: 16,
                        },
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: output,
                            offset: 0,
                            size: 16,
                        },
                    },
                ],
                [1; 3],
            )
            .unwrap();
        graph
            .readback_buffer("observe buffer", output, 0, 16)
            .unwrap();
        (pool, graph.compile().unwrap())
    }

    fn classes(image_count: usize) -> [ResourceClass; 3] {
        [
            ResourceClass::Buffer {
                desc: BufferDesc { size: 512 },
                count: 1,
            },
            ResourceClass::Buffer {
                desc: BufferDesc { size: 64 },
                count: 3,
            },
            ResourceClass::Image {
                desc: fixture::hdr(),
                count: image_count,
            },
        ]
    }

    fn compare(
        expected: &fixture::Expected,
        mut read: impl FnMut(PassHandle, &mut [u8]),
        backend: &str,
    ) -> Vec<Value> {
        expected.readbacks.iter().map(|entry| {
            let mut actual = [0; fixture::IMAGE_BYTES];
            read(entry.pass, &mut actual[..entry.len]);
            assert_eq!(&actual[..entry.len], &entry.bytes[..entry.len], "{backend}: {}", entry.name);
            json!({ "name": entry.name, "actual_bytes": &actual[..entry.len], "expected_bytes": &entry.bytes[..entry.len] })
        }).collect()
    }

    fn run(strict: bool) {
        let mut vulkan = VulkanRuntime::new_with_validation(
            RuntimeConfig {
                execution_slots: 1,
                max_shader_bindings: 16,
                ..Default::default()
            },
            if strict {
                ValidationMode::Required
            } else {
                ValidationMode::Disabled
            },
        )
        .expect("hardware Vulkan and requested validation prerequisites");
        let mut d3d12 = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            capacity: D3D12FrameCapacity {
                executions: 1,
                resources: 16,
                operations: 32,
                buffers: 4,
                buffer_bytes: 512,
                upload_bytes: 16 * 1024,
                readback_bytes: 16 * 1024,
                scratch_bytes: 512,
                provenance_segments_per_resource: 0,
                index_prefixes_per_resource: 0,
                ..Default::default()
            },
        })
        .expect("hardware D3D12 and requested validation prerequisites");
        let vk_info = vulkan.device_info();
        let dx_info = d3d12.device_info();
        assert_ne!(vk_info.device_type, "CPU", "hardware Vulkan required");
        assert_eq!(
            vk_info.device_luid,
            Some(dx_info.luid),
            "same hardware adapter required"
        );
        assert_eq!(vk_info.vendor_id, dx_info.vendor_id);
        assert_eq!(vk_info.device_id, dx_info.device_id);
        let vk_monitor = vulkan.validation_monitor();
        let dx_monitor = d3d12.validation_monitor();
        vulkan
            .prepare_working_set(
                FrameCapacity {
                    graph: fixture::capacity(),
                    work_slots: 1,
                    ..Default::default()
                },
                &classes(3),
            )
            .unwrap();
        let image_classes = [D3D12ImageClass {
            desc: fixture::hdr(),
            count: 3,
        }];
        d3d12
            .prepare_graph_working_set(None, &image_classes, &[], &[])
            .unwrap();
        let (unprepared_pool, unprepared) = unprepared_buffer_plan();
        assert_eq!(
            vulkan
                .submit(&unprepared)
                .err()
                .expect("unprepared compute rejected")
                .kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(
            d3d12
                .prepare(&unprepared)
                .err()
                .expect("unprepared compute rejected")
                .kind(),
            D3D12RuntimeErrorKind::CapacityExhausted
        );
        drop((unprepared, unprepared_pool));
        vulkan
            .prepare_compute_programs(&fixture::programs())
            .unwrap();
        d3d12
            .prepare_graph_working_set(None, &image_classes, &[], &fixture::programs())
            .unwrap();
        let (_pool, shared, expected) = plan();
        assert_eq!(
            vulkan
                .submit(&shared)
                .err()
                .expect("insufficient prepared image count")
                .kind(),
            RuntimeErrorKind::CapacityExhausted,
        );
        assert_eq!(
            d3d12
                .prepare(&shared)
                .err()
                .expect("insufficient prepared image count")
                .kind(),
            D3D12RuntimeErrorKind::CapacityExhausted,
        );
        vulkan
            .prepare_working_set(
                FrameCapacity {
                    graph: fixture::capacity(),
                    work_slots: 1,
                    ..Default::default()
                },
                &classes(4),
            )
            .unwrap();
        d3d12
            .prepare_graph_working_set(
                None,
                &[D3D12ImageClass {
                    desc: fixture::hdr(),
                    count: 4,
                }],
                &[],
                &fixture::programs(),
            )
            .unwrap();

        let recording = d3d12
            .prepare(&shared)
            .expect("record full Compute graph without submission");
        assert_eq!(
            d3d12
                .prepare(&shared)
                .err()
                .expect("sole recording lease held")
                .kind(),
            D3D12RuntimeErrorKind::CapacityExhausted
        );
        drop(recording);
        drop(
            d3d12
                .prepare(&shared)
                .expect("cancel returns Compute descriptors and images"),
        );
        for _ in 0..2 {
            let mut submitted = vulkan.submit(&shared).expect("Vulkan Compute graph submit");
            let mut result = submitted
                .wait(WAIT)
                .unwrap()
                .expect("Vulkan Compute completion");
            compare(
                &expected,
                |pass, bytes| result.readback_into(pass, bytes).unwrap(),
                "Vulkan reuse",
            );
            drop((result, submitted));
            let mut submitted = d3d12.submit(&shared).expect("D3D12 Compute graph submit");
            let mut result = submitted
                .wait(WAIT)
                .unwrap()
                .expect("D3D12 Compute completion");
            compare(
                &expected,
                |pass, bytes| result.readback_into(pass, bytes).unwrap(),
                "D3D12 reuse",
            );
        }
        drop(shared);

        // Lose every public plan/runtime handle before observing completion.
        // The GPU need not still be busy at that CPU instant; no such claim is made.
        let (vk_pool, vk_plan, vk_expected) = plan();
        let mut vk_submission = vulkan.submit(&vk_plan).unwrap();
        drop(vk_plan);
        drop(vulkan);
        let mut vk_result = vk_submission
            .wait(WAIT)
            .unwrap()
            .expect("Vulkan retained owners");
        let vk_output = compare(
            &vk_expected,
            |pass, bytes| vk_result.readback_into(pass, bytes).unwrap(),
            "Vulkan escaped owners",
        );
        drop((vk_result, vk_submission, vk_pool));

        let (dx_pool, dx_plan, dx_expected) = plan();
        let mut dx_submission = d3d12.submit(&dx_plan).unwrap();
        drop(dx_plan);
        d3d12.shutdown().unwrap();
        let mut dx_result = dx_submission
            .wait(WAIT)
            .unwrap()
            .expect("D3D12 retained owners");
        let dx_output = compare(
            &dx_expected,
            |pass, bytes| dx_result.readback_into(pass, bytes).unwrap(),
            "D3D12 escaped owners",
        );
        drop((dx_result, dx_submission, dx_pool));
        assert_eq!(
            vk_output, dx_output,
            "independent expectations and APIs agree"
        );
        let vk_report = vk_monitor.report();
        let dx_report = dx_monitor.snapshot();
        let report = json!({
            "scope": "consumer-owned-compute-common-graph", "strict": strict,
            "configuration": { "development_validation": cfg!(feature="development-validation"), "graph_diagnostics": cfg!(feature="graph-diagnostics"), "vulkan_diagnostics": cfg!(feature="vulkan-diagnostics"), "d3d12_diagnostics": cfg!(feature="d3d12-diagnostics"), "debug_assertions": cfg!(debug_assertions), "external_layer_injection_checked": false },
            "adapter": { "name": vk_info.name, "luid": dx_info.luid, "vendor_id": dx_info.vendor_id, "device_id": dx_info.device_id, "vulkan_driver": vk_info.driver_info, "d3d12_driver": dx_info.driver_version },
            "extent": [fixture::WIDTH, fixture::HEIGHT], "custom_workgroup_size": [4,4,1], "custom_workgroups": [1,2,1],
            "compared_bytes_per_backend_per_run": fixture::READBACK_BYTES, "successful_runs_per_backend": 3, "same_compiled_graph_runs_per_backend": 2,
            "missing_preparation_rejected": true, "insufficient_image_capacity_rejected_then_explicitly_grown": true, "d3d12_record_cancel_reuse": true, "native_runs_sequential": true,
            "runtime_and_all_public_plan_handles_dropped_before_completion_observation": true,
            "vulkan_readbacks": vk_output, "d3d12_readbacks": dx_output,
            "validation": {
                "vulkan": { "requested": format!("{:?}", vk_report.requested), "core": vk_report.core_validation, "sync": vk_report.synchronization_validation, "warnings": vk_report.warnings, "errors": vk_report.errors, "callback_failures": vk_report.callback_failures, "dropped_problems": vk_report.dropped_problems, "dropped_layer_statuses": vk_report.dropped_layer_statuses, "problems": vk_report.problems, "layer_status_messages": vk_report.layer_status_messages },
                "d3d12": { "debug_layer": dx_report.debug_layer_enabled, "gbv": dx_report.gpu_based_validation_enabled, "teardown_checked": dx_report.teardown_checked, "warnings": dx_report.warnings, "errors": dx_report.errors, "corruption": dx_report.corruption, "callback_failures": dx_report.callback_failures, "dropped_messages": dx_report.dropped_messages, "retained_message_count": dx_report.messages.len() }
            }
        });
        eprintln!("COMPUTE_GRAPH_REPORT={report}");
        assert!(vk_report.is_clean(), "Vulkan diagnostics: {report}");
        assert!(
            dx_report.is_clean(),
            "D3D12 diagnostics: {dx_report:?}; {report}"
        );
        if strict {
            assert_eq!(vk_report.core_validation, Some(true), "{report}");
            assert_eq!(vk_report.synchronization_validation, Some(true), "{report}");
            assert_eq!(vk_report.dropped_layer_statuses, 0, "{report}");
            assert!(
                dx_report.debug_layer_enabled
                    && dx_report.gpu_based_validation_enabled
                    && dx_report.teardown_checked,
                "{report}"
            );
        } else {
            assert_eq!(vk_report.requested, ValidationMode::Disabled);
            assert_eq!(vk_report.core_validation, None);
            assert_eq!(vk_report.synchronization_validation, None);
            assert!(!dx_report.debug_layer_enabled && !dx_report.gpu_based_validation_enabled);
        }
    }

    const BINDING_BUFFER_BYTES: u64 = 832;
    const BINDING_PROGRAMS: [ManagedProgram; 4] = [
        ManagedProgram::uniform_readback(),
        ManagedProgram::storage_readback(),
        ManagedProgram::sampled_readback(),
        ManagedProgram::sampled_float_readback(),
    ];

    fn binding_image_descriptions() -> [ImageDesc; 3] {
        let rgba8 = ImageDesc::rgba8(2, 2);
        [
            rgba8,
            ImageDesc {
                format: ImageFormat::Rgba16Float,
                ..rgba8
            },
            ImageDesc::depth32(2, 2),
        ]
    }

    struct BindingReadbacks {
        passes: [PassHandle; 4],
        uniform: [u8; 16],
        rgba8: [u8; 16],
        rgba16: [f32; 16],
        depth: f32,
    }

    impl BindingReadbacks {
        fn verify(&self, mut read: impl FnMut(PassHandle, &mut [u8])) {
            let [storage, rgba8, rgba16, depth] = self.passes;
            let mut short = [0; 16];
            read(storage, &mut short);
            assert_eq!(short, self.uniform);
            read(rgba8, &mut short);
            assert_eq!(short, self.rgba8);

            let mut float = [0; 64];
            read(rgba16, &mut float);
            let rgba16_bits: [u32; 16] =
                std::array::from_fn(|index| u32::from_le_bytes(float.as_chunks::<4>().0[index]));
            assert_eq!(rgba16_bits, self.rgba16.map(f32::to_bits));

            read(depth, &mut float);
            for pixel in float.as_chunks::<16>().0 {
                let red = u32::from_le_bytes(pixel[..4].try_into().unwrap());
                assert_eq!(red, self.depth.to_bits());
            }
        }
    }

    fn binding_plan(pool: &mut GraphPool, newer: bool) -> (CompiledGraph, BindingReadbacks) {
        let mut graph = pool.begin().unwrap();
        let [uniform, intermediate, output] = ["uniform", "intermediate", "output"].map(|name| {
            graph
                .buffer(
                    name,
                    BufferDesc {
                        size: BINDING_BUFFER_BYTES,
                    },
                )
                .unwrap()
        });
        let uniform_bytes: [u8; 16] = std::array::from_fn(|index| {
            if newer {
                251 - index as u8
            } else {
                3 * index as u8
            }
        });
        graph
            .upload_buffer("uniform at offset", uniform, 256, &uniform_bytes)
            .unwrap();
        let buffer = |group, buffer: BufferHandle, offset, size| Binding {
            group,
            binding: 0,
            resource: BindingResource::Buffer {
                buffer,
                offset,
                size,
            },
        };
        graph
            .dispatch(
                "produce storage range",
                &BINDING_PROGRAMS[0],
                &[
                    buffer(0, uniform, 256, 16),
                    buffer(1, intermediate, 256, 16),
                ],
                [1; 3],
            )
            .unwrap();
        graph
            .dispatch(
                "consume produced storage range",
                &BINDING_PROGRAMS[1],
                &[buffer(0, intermediate, 256, 16), buffer(1, output, 256, 16)],
                [1; 3],
            )
            .unwrap();

        let [rgba8_desc, rgba16_desc, depth_desc] = binding_image_descriptions();
        let rgba8 = graph.image("RGBA8 sample source", rgba8_desc).unwrap();
        let rgba16 = graph.image("RGBA16F sample source", rgba16_desc).unwrap();
        let depth = graph.image("D32 sample source", depth_desc).unwrap();
        let mut rgba8_bytes = [
            5, 17, 29, 43, 61, 79, 97, 113, 127, 149, 173, 199, 211, 223, 239, 251,
        ];
        // Exact finite binary16 values. The expected binary32 values below are independent.
        let mut half: [u16; 16] = [
            0xc000, 0xbc00, 0xb800, 0xb400, 0x0000, 0x3400, 0x3800, 0x3c00, 0x3e00, 0x4000, 0x4200,
            0x4400, 0x4800, 0x4c00, 0x5000, 0x5400,
        ];
        let mut expanded = [
            -2.0, -1.0, -0.5, -0.25, 0.0, 0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 8.0, 16.0, 32.0, 64.0,
        ];
        if newer {
            rgba8_bytes.reverse();
            half.reverse();
            expanded.reverse();
        }
        let half_bytes: Vec<u8> = half.into_iter().flat_map(u16::to_le_bytes).collect();
        let depth_value = if newer { 0.75 } else { 0.25 };
        graph
            .upload_image("upload RGBA8", rgba8, &rgba8_bytes)
            .unwrap();
        graph
            .upload_image("upload RGBA16F", rgba16, &half_bytes)
            .unwrap();
        graph
            .attachment_pass(
                "clear depth",
                RasterAttachments {
                    colors: &[],
                    color_resolves: &[],
                    depth: Some(Attachment {
                        view: graph.image_view(depth, 0, 0).unwrap(),
                        load: LoadOp::Clear(depth_value),
                        store: StoreOp::Store,
                    }),
                },
            )
            .unwrap();

        for (name, image, program, offset, size) in [
            ("sample RGBA8", rgba8, BINDING_PROGRAMS[2], 0, 16),
            ("sample RGBA16F", rgba16, BINDING_PROGRAMS[3], 512, 64),
            ("sample D32", depth, BINDING_PROGRAMS[3], 768, 64),
        ] {
            graph
                .dispatch(
                    name,
                    &program,
                    &[
                        buffer(1, output, offset, size),
                        Binding {
                            group: 0,
                            binding: 0,
                            resource: BindingResource::ImageView(
                                graph.image_view(image, 0, 0).unwrap(),
                            ),
                        },
                        Binding {
                            group: 0,
                            binding: 1,
                            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                        },
                    ],
                    [1; 3],
                )
                .unwrap();
        }
        let passes = [
            ("storage at offset", 256, 16),
            ("packed RGBA8", 0, 16),
            ("expanded RGBA16F", 512, 64),
            ("sampled D32", 768, 64),
        ]
        .map(|(name, offset, size)| graph.readback_buffer(name, output, offset, size).unwrap());
        (
            graph.compile().unwrap(),
            BindingReadbacks {
                passes,
                uniform: uniform_bytes,
                rgba8: rgba8_bytes,
                rgba16: expanded,
                depth: depth_value,
            },
        )
    }

    fn binding_readbacks(strict: bool) {
        let graph_capacity = GraphCapacity {
            resources: 6,
            passes: 16,
            accesses: 32,
            bindings: 16,
            attachments: 1,
            draws: 0,
            upload_bytes: 128,
            ..GraphCapacity::default()
        };
        let mut pool = GraphPool::new(graph_capacity, 2).unwrap();
        let (older, older_readbacks) = binding_plan(&mut pool, false);
        let (newer, newer_readbacks) = binding_plan(&mut pool, true);
        let descs = binding_image_descriptions();

        let mut vulkan = VulkanRuntime::new_with_validation(
            RuntimeConfig {
                execution_slots: 2,
                max_shader_bindings: 16,
                ..Default::default()
            },
            if strict {
                ValidationMode::Required
            } else {
                ValidationMode::Disabled
            },
        )
        .expect("Vulkan hardware and requested validation");
        let monitor = vulkan.validation_monitor();
        let mut classes = vec![ResourceClass::Buffer {
            desc: BufferDesc {
                size: BINDING_BUFFER_BYTES,
            },
            count: 6,
        }];
        classes.extend(descs.map(|desc| ResourceClass::Image { desc, count: 2 }));
        vulkan
            .prepare_working_set(
                FrameCapacity {
                    graph: graph_capacity,
                    work_slots: 2,
                    result_slots: 2,
                    ..Default::default()
                },
                &classes,
            )
            .unwrap();
        vulkan.prepare_compute_programs(&BINDING_PROGRAMS).unwrap();

        let mut d3d12 = D3D12Runtime::new_with_working_set(
            D3D12RuntimeConfig {
                validation: if strict {
                    D3D12ValidationMode::Required
                } else {
                    D3D12ValidationMode::Disabled
                },
                capacity: D3D12FrameCapacity {
                    executions: 2,
                    results: 2,
                    resources: 6,
                    resource_capacity: 12,
                    operations: 16,
                    buffers: 3,
                    buffer_bytes: BINDING_BUFFER_BYTES,
                    upload_bytes: 8192,
                    readback_bytes: 4096,
                    scratch_bytes: BINDING_BUFFER_BYTES,
                    ..Default::default()
                },
            },
            &[D3D12BufferClass {
                size: BINDING_BUFFER_BYTES,
                count: 6,
            }],
            &descs.map(|desc| D3D12ImageClass { desc, count: 2 }),
            &[],
            &BINDING_PROGRAMS,
        )
        .expect("D3D12 hardware and requested validation");
        assert_ne!(vulkan.device_info().device_type, "CPU");
        assert_eq!(
            vulkan.device_info().device_luid,
            Some(d3d12.device_info().luid)
        );
        let dx_monitor = d3d12.validation_monitor();

        // Submit both content versions before waiting, then return the newer
        // result first. Its resources can be reused while the older result is held.
        let mut old_vk = vulkan.submit(&older).unwrap();
        let mut new_vk = vulkan.submit(&newer).unwrap();
        {
            let mut result = new_vk.wait(WAIT).unwrap().expect("new Vulkan result");
            newer_readbacks.verify(|pass, bytes| result.readback_into(pass, bytes).unwrap());
        }
        drop(new_vk);
        let mut reused_vk = vulkan.submit(&newer).unwrap();

        let mut old_dx = d3d12.submit(&older).unwrap();
        let mut new_dx = d3d12.submit(&newer).unwrap();
        {
            let mut result = new_dx.wait(WAIT).unwrap().expect("new D3D12 result");
            newer_readbacks.verify(|pass, bytes| result.readback_into(pass, bytes).unwrap());
        }
        drop(new_dx);
        let mut reused_dx = d3d12.submit(&newer).unwrap();

        drop((older, newer, pool, vulkan));
        d3d12.shutdown().unwrap();
        for (submission, expected) in [
            (&mut old_vk, &older_readbacks),
            (&mut reused_vk, &newer_readbacks),
        ] {
            let mut result = submission
                .wait(WAIT)
                .unwrap()
                .expect("escaped Vulkan result");
            expected.verify(|pass, bytes| result.readback_into(pass, bytes).unwrap());
        }
        for (submission, expected) in [
            (&mut old_dx, &older_readbacks),
            (&mut reused_dx, &newer_readbacks),
        ] {
            let mut result = submission
                .wait(WAIT)
                .unwrap()
                .expect("escaped D3D12 result");
            expected.verify(|pass, bytes| result.readback_into(pass, bytes).unwrap());
        }
        drop((old_vk, reused_vk, old_dx, reused_dx));

        let vk = monitor.report();
        let dx = dx_monitor.snapshot();
        assert!(vk.is_clean(), "{vk:?}");
        assert!(dx.is_clean(), "{dx:?}");
        if strict {
            assert_eq!(vk.core_validation, Some(true), "{vk:?}");
            assert_eq!(vk.synchronization_validation, Some(true), "{vk:?}");
            assert!(
                dx.debug_layer_enabled && dx.gpu_based_validation_enabled,
                "{dx:?}"
            );
            assert!(dx.teardown_checked, "{dx:?}");
        } else {
            assert_eq!(vk.requested, ValidationMode::Disabled);
            assert!(!dx.debug_layer_enabled && !dx.gpu_based_validation_enabled);
        }
    }

    #[cfg(all(feature = "d3d12-diagnostics", feature = "vulkan-diagnostics"))]
    #[test]
    #[ignore = "requires Vulkan Core/SyncVal and D3D12 Debug Layer/GBV on the same hardware adapter"]
    fn managed_binding_graph_readbacks_strict() {
        binding_readbacks(true);
    }

    #[cfg(not(any(
        feature = "development-validation",
        feature = "d3d12-diagnostics",
        feature = "vulkan-diagnostics",
        feature = "graph-diagnostics"
    )))]
    #[test]
    #[ignore = "requires optimized Vulkan/D3D12 hardware without diagnostics"]
    fn managed_binding_graph_readbacks_shipping() {
        if cfg!(debug_assertions) {
            panic!("shipping acceptance requires --release");
        }
        binding_readbacks(false);
    }

    #[cfg(all(feature = "d3d12-diagnostics", feature = "vulkan-diagnostics"))]
    #[test]
    #[ignore = "requires same-adapter hardware, Vulkan Core/SyncVal and D3D12 Debug Layer/GBV"]
    fn owned_compute_common_graph_strict() {
        run(true);
    }

    #[cfg(not(any(
        feature = "development-validation",
        feature = "d3d12-diagnostics",
        feature = "vulkan-diagnostics",
        feature = "graph-diagnostics"
    )))]
    #[test]
    #[ignore = "requires same-adapter Vulkan/D3D12 hardware; run optimized with diagnostics excluded"]
    fn owned_compute_common_graph_shipping() {
        if cfg!(debug_assertions) {
            panic!("shipping acceptance requires --release");
        }
        run(false);
    }
}

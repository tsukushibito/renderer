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
        Binding, BindingResource, BufferDesc, CompiledGraph, GraphPool, PassHandle,
    };
    use renderer::{
        D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
        D3D12RuntimeErrorKind, D3D12ValidationMode, FrameCapacity, ResourceClass, RuntimeConfig,
        RuntimeErrorKind, ValidationMode, VulkanRuntime,
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

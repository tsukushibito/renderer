//! A public-API acceptance of one compiled buffer Graph on both native APIs.
//! Native prerequisites are deliberately failures, never successful skips.

#![cfg(all(
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

use std::time::Duration;

use renderer::graph::{
    Binding, BufferDesc, BufferHandle, CompiledGraph, Graph, GraphCapacity, GraphPool, PassHandle,
};
use renderer::program::{ManagedProgram, ManagedRasterProgram, SamplerDescriptor};
use renderer::{
    D3D12Runtime, D3D12RuntimeConfig, D3D12RuntimeErrorKind, D3D12ValidationMode, ResourceClass,
    RuntimeConfig, RuntimeErrorKind, ValidationMode, VulkanRuntime,
};
use serde_json::{Value, json};

struct ExpectedReadback {
    pass: PassHandle,
    name: &'static str,
    bytes: Vec<u8>,
}

fn append_owned_dispatch(graph: &mut Graph) -> BufferHandle {
    let uniform = graph
        .buffer("dispatch uniform", BufferDesc { size: 128 })
        .unwrap();
    let output = graph
        .buffer("dispatch output", BufferDesc { size: 128 })
        .unwrap();
    graph
        .upload_buffer("dispatch input", uniform, 0, &[7; 16])
        .unwrap();
    graph
        .dispatch(
            "managed dispatch",
            &ManagedProgram::uniform_readback(),
            &[
                Binding {
                    group: 0,
                    binding: 0,
                    resource: renderer::graph::BindingResource::Buffer {
                        buffer: uniform,
                        offset: 0,
                        size: 32,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: renderer::graph::BindingResource::Buffer {
                        buffer: output,
                        offset: 0,
                        size: 32,
                    },
                },
            ],
            [1, 1, 1],
        )
        .unwrap();
    output
}

fn shared_buffer_graph() -> (GraphPool, CompiledGraph, Vec<ExpectedReadback>) {
    let mut graph_pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = graph_pool.begin().unwrap();
    let source = graph.buffer("source", BufferDesc { size: 128 }).unwrap();
    let intermediate = graph
        .buffer("intermediate", BufferDesc { size: 160 })
        .unwrap();
    let destination = graph
        .buffer("destination", BufferDesc { size: 128 })
        .unwrap();
    let source_bytes: Vec<u8> = (0..128)
        .map(|index| ((index * 37 + 11) ^ (index >> 2)) as u8)
        .collect();
    let mut intermediate_bytes = vec![0xcc; 160];
    let mut destination_bytes = vec![0x5a; 128];
    graph
        .upload_buffer("source contents", source, 0, &source_bytes)
        .unwrap();
    graph
        .upload_buffer(
            "intermediate contents",
            intermediate,
            0,
            &intermediate_bytes,
        )
        .unwrap();
    graph
        .upload_buffer("destination contents", destination, 0, &destination_bytes)
        .unwrap();

    graph
        .copy_buffer("middle region", source, 16, intermediate, 32, 64)
        .unwrap();
    intermediate_bytes[32..96].copy_from_slice(&source_bytes[16..80]);
    graph
        .copy_buffer("prefix region", source, 80, intermediate, 8, 24)
        .unwrap();
    intermediate_bytes[8..32].copy_from_slice(&source_bytes[80..104]);
    // Nonoverlapping self-copy is legal in the shared Graph. A backend must
    // preserve these bytes even if its native states require a scratch buffer.
    graph
        .copy_buffer("disjoint self copy", intermediate, 8, intermediate, 112, 16)
        .unwrap();
    intermediate_bytes.copy_within(8..24, 112);
    graph
        .copy_buffer("final region", intermediate, 8, destination, 16, 112)
        .unwrap();
    destination_bytes[16..128].copy_from_slice(&intermediate_bytes[8..120]);

    let intermediate_read = graph
        .readback_buffer("whole intermediate", intermediate, 0, 160)
        .unwrap();
    let destination_read = graph
        .readback_buffer("destination range", destination, 8, 112)
        .unwrap();
    let source_read = graph
        .readback_buffer("source range", source, 20, 36)
        .unwrap();
    // Unsupported execution must be checked only for live operations. This
    // branch is fully culled and must not acquire either physical buffer.
    append_owned_dispatch(&mut graph);
    append_owned_textured_raster(&mut graph);
    append_owned_sampled_dispatch(&mut graph);
    (
        graph_pool,
        graph.compile().unwrap(),
        vec![
            ExpectedReadback {
                pass: intermediate_read,
                name: "whole intermediate",
                bytes: intermediate_bytes,
            },
            ExpectedReadback {
                pass: destination_read,
                name: "destination range",
                bytes: destination_bytes[8..120].to_vec(),
            },
            ExpectedReadback {
                pass: source_read,
                name: "source range",
                bytes: source_bytes[20..56].to_vec(),
            },
        ],
    )
}

fn append_owned_textured_raster(graph: &mut Graph) -> renderer::graph::ImageHandle {
    use renderer::graph::{
        Attachment, Binding, BindingResource, ImageDesc, LoadOp, RasterAttachments, RasterDraw,
        RasterParameters, StoreOp,
    };
    let vertices = graph
        .buffer("textured vertices", BufferDesc { size: 28 })
        .unwrap();
    let indices = graph
        .buffer("textured indices", BufferDesc { size: 12 })
        .unwrap();
    let uniform = graph
        .buffer("textured offset", BufferDesc { size: 16 })
        .unwrap();
    graph
        .upload_buffer("textured vertex bytes", vertices, 0, &[0; 28])
        .unwrap();
    graph
        .upload_buffer("textured index bytes", indices, 0, &[0; 12])
        .unwrap();
    graph
        .upload_buffer("textured offset bytes", uniform, 0, &[0; 16])
        .unwrap();
    let source = graph
        .image("textured source", ImageDesc::rgba8(2, 2))
        .unwrap();
    let output = graph
        .image("textured output", ImageDesc::rgba8(2, 2))
        .unwrap();
    let depth = graph
        .image("textured depth", ImageDesc::depth32(2, 2))
        .unwrap();
    graph
        .upload_image("textured pixels", source, &[255; 16])
        .unwrap();
    let bindings = [
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
            resource: BindingResource::ImageView(graph.image_view(source, 0, 0).unwrap()),
        },
        Binding {
            group: 1,
            binding: 1,
            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
        },
    ];
    let bindings = graph
        .raster_bindings(&ManagedRasterProgram::textured_mesh(), &bindings)
        .unwrap();
    graph
        .raster(
            "textured Raster",
            &ManagedRasterProgram::textured_mesh(),
            [RasterDraw {
                vertices,
                indices,
                index_count: 3,
                parameters: RasterParameters::default().into(),
                bindings: Some(bindings),
            }],
            RasterAttachments {
                colors: &[Attachment {
                    view: graph.image_view(output, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    output
}

fn append_owned_sampled_dispatch(graph: &mut Graph) -> BufferHandle {
    use renderer::graph::{Binding, BindingResource, ImageDesc};
    let source = graph
        .image("sampled source", ImageDesc::rgba8(2, 2))
        .unwrap();
    let output = graph
        .buffer("sampled output", BufferDesc { size: 64 })
        .unwrap();
    graph
        .upload_image("sampled input", source, &[127; 16])
        .unwrap();
    graph
        .dispatch(
            "sampled dispatch",
            &ManagedProgram::sampled_float_readback(),
            &[
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::ImageView(graph.image_view(source, 0, 0).unwrap()),
                },
                Binding {
                    group: 0,
                    binding: 1,
                    resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: output,
                        offset: 0,
                        size: 64,
                    },
                },
            ],
            [1; 3],
        )
        .unwrap();
    output
}

fn checksum(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

fn emit_report(report: &Value) {
    eprintln!("M4_BUFFER_REPORT={report}");
    if let Some(path) = std::env::var_os("RENDERER_M4_BUFFER_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(report).unwrap())
            .expect("write requested M4 buffer acceptance report");
    }
}

fn run_shared_graph(strict_validation: bool) {
    let mut vulkan = VulkanRuntime::new_with_validation(
        RuntimeConfig::default(),
        if strict_validation {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )
    .expect("Vulkan hardware and requested validation are required");
    let mut d3d12 = D3D12Runtime::new(D3D12RuntimeConfig {
        validation: if strict_validation {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        },
        ..Default::default()
    })
    .expect("D3D12 hardware and requested validation are required");
    let vulkan_info = vulkan.device_info();
    let d3d12_info = d3d12.device_info();
    assert_ne!(
        vulkan_info.device_type, "CPU",
        "hardware Vulkan is required"
    );
    assert_eq!(
        vulkan_info.device_luid,
        Some(d3d12_info.luid),
        "both selected Devices must identify the same DXGI hardware adapter"
    );
    assert_eq!(vulkan_info.vendor_id, d3d12_info.vendor_id);
    assert_eq!(vulkan_info.device_id, d3d12_info.device_id);
    let vulkan_monitor = vulkan.validation_monitor();
    let d3d12_monitor = d3d12.validation_monitor();

    let (_graph_pool, plan, expected) = shared_buffer_graph();
    vulkan
        .prepare_working_set(
            Default::default(),
            &[
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 128 },
                    count: 2,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 160 },
                    count: 1,
                },
            ],
        )
        .expect("prepare all native resource classes before submitting the common Graph");
    let prepared_resources = vulkan.stats().resources_created;
    #[cfg(feature = "memory-profiling")]
    let mut rejection_native_events = Some([[0_u64; 9]; 2]);
    #[cfg(not(feature = "memory-profiling"))]
    let rejection_native_events: Option<[[u64; 9]; 2]> = None;
    for mode in 0..2 {
        let mut rejected_pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut rejected_graph = rejected_pool.begin().unwrap();
        match mode {
            0 => {
                let output = append_owned_dispatch(&mut rejected_graph);
                rejected_graph
                    .readback_buffer("live dispatch result", output, 0, 16)
                    .unwrap();
            }
            _ => {
                let output = append_owned_sampled_dispatch(&mut rejected_graph);
                rejected_graph
                    .readback_buffer("live sampled result", output, 0, 64)
                    .unwrap();
            }
        }
        let rejected = rejected_graph.compile().unwrap();
        let before = vulkan.stats();
        #[cfg(feature = "memory-profiling")]
        let before_native = renderer::measurement::Snapshot::now();
        assert_eq!(
            vulkan
                .submit(&rejected)
                .err()
                .expect("Compute Program was not prepared in this buffer-only working set")
                .kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(
            d3d12
                .submit(&rejected)
                .err()
                .expect("the unprepared Program or image working set must be rejected")
                .kind(),
            D3D12RuntimeErrorKind::CapacityExhausted
        );
        #[cfg(feature = "memory-profiling")]
        {
            let events = renderer::measurement::Snapshot::now().since(before_native);
            assert!(!events.overflowed);
            assert_eq!(
                &events.native[..renderer::measurement::NativeEvent::Reset as usize],
                &[0; 8],
                "rejection created or destroyed a native object/resource: {events:?}"
            );
            rejection_native_events.as_mut().unwrap()[mode] = events.native;
        }
        assert_eq!(vulkan.stats().resources_created, before.resources_created);
        assert_eq!(vulkan.stats().resources_reused, before.resources_reused);
        drop((rejected, rejected_pool));
    }
    // A rejected plan must return reusable recording/result reservations;
    // the following ordinary Graph still executes successfully on both APIs.
    let pass_count = plan.pass_count();
    let mut vulkan_submission = vulkan.submit(&plan).expect("submit common Graph to Vulkan");
    assert_eq!(
        vulkan.stats().resources_created,
        prepared_resources,
        "culled shader branches acquired physical resources"
    );
    let mut vulkan_result = vulkan_submission
        .wait(Duration::from_secs(30))
        .expect("Vulkan Fence observation")
        .expect("Vulkan buffer Graph must complete within the acceptance timeout");
    // Run native work sequentially to keep this correctness acceptance isolated
    // from work submitted by the comparison backend.
    let mut d3d12_submission = d3d12.submit(&plan).expect("submit common Graph to D3D12");
    // D3D12 command operands now retain their native lifetime independently of
    // this immutable CPU plan. Backend lifetime details have separate tests.
    drop(plan);
    let mut d3d12_result = d3d12_submission
        .wait(Duration::from_secs(30))
        .expect("D3D12 Fence observation")
        .expect("D3D12 buffer Graph must complete within the acceptance timeout");
    let mut readbacks = Vec::new();
    let mut all_bytes_match = true;
    let mut compared_bytes = 0;
    for readback in expected {
        let mut actual_d3d12 = vec![0; readback.bytes.len()];
        d3d12_result
            .readback_into(readback.pass, &mut actual_d3d12)
            .expect("completed D3D12 readback");
        let mut actual_vulkan = vec![0; readback.bytes.len()];
        vulkan_result
            .readback_into(readback.pass, &mut actual_vulkan)
            .expect("completed Vulkan readback");
        let matches = actual_d3d12 == readback.bytes
            && actual_vulkan == readback.bytes
            && actual_d3d12 == actual_vulkan;
        all_bytes_match &= matches;
        compared_bytes += readback.bytes.len();
        readbacks.push(json!({
            "name": readback.name,
            "byte_count": readback.bytes.len(),
            "all_bytes_match": matches,
            // Checksums help identify evidence; acceptance compares every byte.
            "expected_checksum_fnv1a64": checksum(&readback.bytes),
            "vulkan_checksum_fnv1a64": checksum(&actual_vulkan),
            "d3d12_checksum_fnv1a64": checksum(&actual_d3d12),
            "expected_bytes": readback.bytes,
            "vulkan_bytes": actual_vulkan,
            "d3d12_bytes": actual_d3d12,
        }));
    }
    drop(d3d12_result);
    drop(vulkan_result);
    drop(d3d12_submission);
    drop(vulkan_submission);
    d3d12.shutdown().expect("explicit D3D12 shutdown");
    drop(vulkan);
    // Monitors hold diagnostic data only, so this report includes final native
    // Release/Instance destruction rather than extending those lifetimes.
    let d3d12_report = d3d12_monitor.snapshot();
    let vulkan_report = vulkan_monitor.report();
    let report = json!({
        "schema_version": 1,
        "scope": "m4-buffer-common-graph",
        "configuration": {
            "strict_validation": strict_validation,
            "development_validation": cfg!(feature = "development-validation"),
            "d3d12_diagnostics_compiled": cfg!(feature = "d3d12-diagnostics"),
            "vulkan_diagnostics_compiled": cfg!(feature = "vulkan-diagnostics"),
            "graph_diagnostics_compiled": cfg!(feature = "graph-diagnostics"),
            "debug_assertions": cfg!(debug_assertions),
            "external_validation_injection_checked": false,
        },
        "adapter": {
            "same_adapter": true,
            "luid": d3d12_info.luid,
            "vendor_id": d3d12_info.vendor_id,
            "device_id": d3d12_info.device_id,
            "d3d12_name": d3d12_info.name,
            "d3d12_driver_version": d3d12_info.driver_version,
            "vulkan_name": vulkan_info.name,
            "vulkan_driver_version_raw": vulkan_info.driver_version_raw,
            "vulkan_driver_name": vulkan_info.driver_name,
            "vulkan_driver_info": vulkan_info.driver_info,
            "vulkan_device_type": vulkan_info.device_type,
        },
        "graph": {
            "compiled_once": true,
            "native_runs_sequential": true,
            "pass_count": pass_count,
            "upload_passes": 3,
            "copy_passes": 4,
            "disjoint_self_copy_passes": 1,
            "readback_passes": 3,
            "all_bytes_match": all_bytes_match,
            "compared_bytes_per_backend": compared_bytes,
            "readbacks": readbacks,
            "owned_dispatch": {
                "unprepared_program_rejected_by_both_backends": true,
                "culled_branch_does_not_prevent_execution": true,
                "working_set": "buffer transfers only; no Compute Programs prepared",
            },
            "shader_bindings": {
                "unprepared_sampled_plan_rejected_by_both_backends": true,
                "vulkan_rejection_resource_create_reuse_unchanged": true,
                "vulkan_culled_branches_no_new_resources": true,
                "rejection_native_event_counters_checked": cfg!(feature = "memory-profiling"),
                "rejection_order":["buffer_dispatch_plan","sampled_dispatch_plan_with_unprepared_image"],
                "d3d12_rejection_kinds":["CapacityExhausted","CapacityExhausted"],
                "rejection_native_events_object_create_destroy_resource_create_destroy_memory_alloc_free_descriptor_alloc_free_reset": rejection_native_events,
                "culled_branches_do_not_prevent_common_execution": true,
                "working_set_has_prepared_compute_programs": false,
            },
        },
        "validation": {
            "d3d12": {
                "debug_layer_enabled": d3d12_report.debug_layer_enabled,
                "gpu_based_validation_enabled": d3d12_report.gpu_based_validation_enabled,
                "gbv_shader_exercised": false,
                "teardown_checked": d3d12_report.teardown_checked,
                "warnings": d3d12_report.warnings,
                "errors": d3d12_report.errors,
                "corruption": d3d12_report.corruption,
                "callback_failures": d3d12_report.callback_failures,
                "dropped_messages": d3d12_report.dropped_messages,
                "raw_report": format!("{d3d12_report:?}"),
            },
            "vulkan": {
                "requested": format!("{:?}", vulkan_report.requested),
                "core_validation": vulkan_report.core_validation,
                "synchronization_validation": vulkan_report.synchronization_validation,
                "native_owners_dropped": true,
                "teardown_checked": strict_validation,
                "warnings": vulkan_report.warnings,
                "errors": vulkan_report.errors,
                "callback_failures": vulkan_report.callback_failures,
                "dropped_problems": vulkan_report.dropped_problems,
                "dropped_layer_statuses": vulkan_report.dropped_layer_statuses,
                "problems": vulkan_report.problems,
                "layer_status_messages": vulkan_report.layer_status_messages,
            },
        },
    });
    emit_report(&report);
    assert!(all_bytes_match, "every readback byte must match: {report}");
    assert!(d3d12_report.is_clean(), "D3D12 diagnostics: {report}");
    assert!(vulkan_report.is_clean(), "Vulkan diagnostics: {report}");
    if strict_validation {
        assert!(d3d12_report.debug_layer_enabled, "{report}");
        assert!(d3d12_report.gpu_based_validation_enabled, "{report}");
        assert!(d3d12_report.teardown_checked, "{report}");
        assert_eq!(vulkan_report.core_validation, Some(true), "{report}");
        assert_eq!(
            vulkan_report.synchronization_validation,
            Some(true),
            "{report}"
        );
        assert_eq!(vulkan_report.dropped_layer_statuses, 0, "{report}");
    } else {
        assert!(!d3d12_report.debug_layer_enabled, "{report}");
        assert!(!d3d12_report.gpu_based_validation_enabled, "{report}");
        assert_eq!(
            vulkan_report.requested,
            ValidationMode::Disabled,
            "{report}"
        );
        assert_eq!(vulkan_report.core_validation, None, "{report}");
        assert_eq!(vulkan_report.synchronization_validation, None, "{report}");
    }
}

#[cfg(all(feature = "d3d12-diagnostics", feature = "vulkan-diagnostics"))]
#[test]
#[ignore = "requires D3D12 hardware, Debug Layer/GBV and Vulkan Core/SyncVal on the same adapter"]
fn m4_buffer_shared_graph_strict() {
    run_shared_graph(true);
}

#[cfg(not(any(
    feature = "development-validation",
    feature = "d3d12-diagnostics",
    feature = "vulkan-diagnostics",
    feature = "graph-diagnostics"
)))]
#[test]
#[ignore = "requires D3D12 and Vulkan hardware on the same adapter; run optimized without diagnostics"]
fn m4_buffer_shared_graph_shipping() {
    if cfg!(debug_assertions) {
        panic!("shipping content acceptance requires --release with debug assertions disabled");
    }
    run_shared_graph(false);
}

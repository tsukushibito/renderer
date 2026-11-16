//! Consumer-defined graphics assets use the standard Graph and lifetime paths.

#[allow(dead_code)]
#[path = "../examples/support/compute_fixture.rs"]
mod compute_fixture;
#[allow(dead_code)]
#[path = "support/custom_raster_fixture.rs"]
mod fixture;

use renderer::graph::{
    Attachment, GraphCapacityKind, GraphErrorKind, GraphPool, ImageDesc, LoadOp, RasterAttachments,
    RasterConstants, RasterDraw, StoreOp,
};
use renderer::program::BlendState;

#[test]
fn three_composition_routes_capture_custom_raster_inputs_and_preserve_depth() {
    let exact_first_cycle = [
        [0.25, 0.5, 0.75, 1.0],
        [0.3125, 0.125, 0.5, 1.0],
        [0.46875, 0.0625, 0.25, 1.0],
        [0.46875, 0.0625, 0.25, 1.0],
        [1.25, 0.25, 1.0, 2.0],
        [0.625, 0.0, 0.0, 0.5],
        [0.34375, 0.0625, 0.5, 1.0],
        [0.25, 0.5, 0.75, 1.0],
    ];
    for (frame, expected) in exact_first_cycle.into_iter().enumerate() {
        assert_eq!(fixture::final_color(frame), expected);
    }
    let mut pool = GraphPool::new(fixture::capacity(), 1).unwrap();
    for frame in 0..fixture::CASES * 2 {
        let (graph, expected) = fixture::build(&mut pool, frame);
        let plan = graph.compile().unwrap();
        assert_eq!(expected.readbacks[1].bytes, expected.readbacks[2].bytes);
        assert_eq!(
            expected
                .readbacks
                .iter()
                .map(|entry| entry.len)
                .sum::<usize>(),
            fixture::READBACK_BYTES
        );
        assert_eq!(
            expected.readbacks[0].bytes == expected.readbacks[1].bytes,
            fixture::overlay_count(frame) == 0,
            "disabled custom stage is identity"
        );
        let names: Vec<_> = plan.ordered_pass_names().collect();
        assert!(names.contains(&"standard output copy"));
        assert_eq!(
            names.contains(&"consumer vertex bytes"),
            fixture::overlay_count(frame) != 0
        );
        assert_eq!(
            names.contains(&"standard vertex bytes"),
            fixture::draw_count(frame) != 0
        );
        drop(plan);
        assert!(pool.is_idle());
    }
    assert_ne!(
        fixture::final_color(2),
        fixture::final_color(6),
        "draw order is observable"
    );
}

#[test]
fn custom_raster_rejects_missing_input_constants_formats_and_required_depth() {
    let mut pool = GraphPool::new(fixture::capacity(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let inputs = fixture::inputs(&mut graph, 1);
    let draws = fixture::overlay_draws(&inputs, 1);
    let color = graph.image("color", fixture::hdr()).unwrap();
    let wrong_color = graph.image("wrong color", ImageDesc::rgba8(3, 7)).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(3, 7)).unwrap();
    let attachment = |graph: &renderer::graph::Graph, image, load| Attachment {
        view: graph.image_view(image, 0, 0).unwrap(),
        load,
        store: StoreOp::Store,
    };
    let depth_attachment = |graph: &renderer::graph::Graph, load| Attachment {
        view: graph.image_view(depth, 0, 0).unwrap(),
        load,
        store: StoreOp::Store,
    };
    let program = fixture::custom_program(BlendState::Alpha);
    let missing = RasterDraw {
        bindings: &inputs.bindings[..3],
        ..draws[0]
    };
    assert_eq!(
        graph
            .raster(
                "missing uniform",
                &program,
                [missing],
                RasterAttachments {
                    colors: &[attachment(&graph, color, LoadOp::Clear([0.0; 4]))],
                    depth: Some(depth_attachment(&graph, LoadOp::Load)),
                }
            )
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    let wrong_constants = RasterDraw {
        parameters: RasterConstants::new(&[0; 4]).unwrap(),
        ..draws[0]
    };
    assert_eq!(
        graph
            .raster(
                "wrong constants",
                &program,
                [wrong_constants],
                RasterAttachments {
                    colors: &[attachment(&graph, color, LoadOp::Clear([0.0; 4]))],
                    depth: Some(depth_attachment(&graph, LoadOp::Load)),
                }
            )
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    assert_eq!(
        graph
            .raster(
                "wrong format",
                &program,
                [draws[0]],
                RasterAttachments {
                    colors: &[attachment(&graph, wrong_color, LoadOp::Clear([0.0; 4]))],
                    depth: Some(depth_attachment(&graph, LoadOp::Load)),
                }
            )
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    for draw_count in [0, 1] {
        assert_eq!(
            graph
                .raster(
                    "missing required depth",
                    &program,
                    &draws[..draw_count],
                    RasterAttachments {
                        colors: &[attachment(&graph, color, LoadOp::Clear([0.0; 4]))],
                        depth: None,
                    }
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
        assert_eq!(
            graph
                .raster(
                    "required depth is not loaded",
                    &program,
                    &draws[..draw_count],
                    RasterAttachments {
                        colors: &[attachment(&graph, color, LoadOp::Clear([0.0; 4]))],
                        depth: Some(depth_attachment(&graph, LoadOp::Clear(1.0))),
                    }
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
    }
}

#[test]
fn custom_raster_draw_capacity_fails_before_implicit_growth() {
    let mut pool = GraphPool::new(
        renderer::graph::GraphCapacity {
            draws: 1,
            ..fixture::capacity()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let inputs = fixture::inputs(&mut graph, 2);
    let draws = fixture::overlay_draws(&inputs, 2);
    let color = graph.image("color", fixture::hdr()).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(3, 7)).unwrap();
    let error = graph
        .raster(
            "over-capacity custom draws",
            &fixture::custom_program(BlendState::Alpha),
            &draws[..2],
            RasterAttachments {
                colors: &[Attachment {
                    view: graph.image_view(color, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
    assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Draws);
}

#[cfg(all(
    windows,
    feature = "vulkan",
    feature = "d3d12",
    any(
        all(feature = "vulkan-diagnostics", feature = "d3d12-diagnostics"),
        not(any(
            feature = "development-validation",
            feature = "vulkan-diagnostics",
            feature = "d3d12-diagnostics",
            feature = "graph-diagnostics"
        ))
    )
))]
mod native {
    use super::*;
    use renderer::{
        D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
        D3D12RuntimeErrorKind, D3D12ValidationMode, FrameCapacity, ResourceClass, RuntimeConfig,
        RuntimeErrorKind, ValidationMode, VulkanRuntime,
        graph::{
            BindingResource, BufferDesc, BufferHandle, CompiledGraph, ImageHandle, PassHandle,
        },
        program::ManagedRasterProgram,
    };
    use serde_json::json;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(30);

    fn resources() -> Vec<ResourceClass> {
        fixture::buffer_classes()
            .iter()
            .map(|&(size, count)| ResourceClass::Buffer {
                desc: BufferDesc { size },
                count: count * 2,
            })
            .chain(
                fixture::image_classes()
                    .into_iter()
                    .map(|(desc, count)| ResourceClass::Image {
                        desc,
                        count: count * 2,
                    }),
            )
            .collect()
    }
    fn images() -> Vec<D3D12ImageClass> {
        fixture::image_classes()
            .into_iter()
            .map(|(desc, count)| D3D12ImageClass { desc, count })
            .collect()
    }
    fn prepare_vk(runtime: &mut VulkanRuntime) {
        runtime
            .prepare_working_set(
                FrameCapacity {
                    graph: fixture::capacity(),
                    work_slots: 2,
                    result_slots: 4,
                    provenance_segments_per_resource: 16,
                    ..Default::default()
                },
                &resources(),
            )
            .unwrap();
        runtime
            .prepare_raster_programs(&fixture::raster_programs())
            .unwrap();
    }
    fn prepare_dx(runtime: &mut D3D12Runtime) {
        runtime
            .prepare_graph_working_set(&images(), &fixture::raster_programs(), &[])
            .unwrap();
    }
    fn compare(
        expected: &compute_fixture::Expected,
        mut read: impl FnMut(PassHandle, &mut [u8]),
    ) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(fixture::READBACK_BYTES);
        for entry in &expected.readbacks {
            let mut actual = [0; fixture::IMAGE_BYTES];
            read(entry.pass, &mut actual[..entry.len]);
            assert_eq!(
                &actual[..entry.len],
                &entry.bytes[..entry.len],
                "{}",
                entry.name
            );
            bytes.extend_from_slice(&actual[..entry.len]);
        }
        bytes
    }
    fn checksum(bytes: &[u8]) -> String {
        format!(
            "{:016x}",
            bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| (hash
                ^ u64::from(*byte))
            .wrapping_mul(0x100000001b3))
        )
    }

    fn exported_input_plan(
        pool: &mut GraphPool,
    ) -> (CompiledGraph, BufferHandle, ImageHandle, PassHandle) {
        let mut graph = pool.begin().unwrap();
        let inputs = fixture::inputs(&mut graph, 2);
        let scene = renderer::standard::graphs::forward(
            &mut graph,
            "pending input base",
            &inputs.opaque,
            renderer::standard::graphs::ForwardConfig {
                color: fixture::hdr(),
                clear_color: [0.25, 0.5, 0.75, 1.0],
                depth: true,
            },
        )
        .unwrap();
        let image = fixture::overlay(&mut graph, scene, &inputs, 2, "pending input Raster");
        let uniform = match inputs.bindings[3].resource {
            BindingResource::Buffer { buffer, .. } => buffer,
            _ => unreachable!("fixture uniform"),
        };
        let texture = match inputs.bindings[1].resource {
            BindingResource::ImageView(view) => view.image(),
            _ => unreachable!("fixture texture"),
        };
        graph.export_buffer(uniform).unwrap();
        graph.export_image(texture).unwrap();
        let readback = graph
            .readback_image("pending original pixels", image)
            .unwrap();
        (graph.compile().unwrap(), uniform, texture, readback)
    }

    // A small acceptance adapter over the identical public methods. The first
    // Raster must consume its captured input revision before the queued update,
    // even after both registrations are deleted while completion is unobserved.
    macro_rules! pending_input_updates {
        ($runtime:expr, $pool:expr) => {{
            let runtime = &mut $runtime;
            let (plan, uniform, texture, pass) = exported_input_plan(&mut $pool);
            let mut pending = runtime.submit(&plan).unwrap();
            let old_uniform = pending.export_buffer(uniform).unwrap();
            let old_texture = pending.export_image(texture).unwrap();
            let mut writer = $pool.begin().unwrap();
            let uniform = writer
                .import_buffer("update pending Uniform", &old_uniform)
                .unwrap();
            writer
                .upload_buffer(
                    "new Uniform revision",
                    uniform,
                    0,
                    &compute_fixture::vec4_bytes([0.5; 4]),
                )
                .unwrap();
            writer.export_buffer(uniform).unwrap();
            let texture = writer
                .import_image("update pending texture", &old_texture)
                .unwrap();
            writer
                .upload_image(
                    "new texture revision",
                    texture,
                    &compute_fixture::image_bytes(&[[0.25; 4]; fixture::PIXELS]),
                )
                .unwrap();
            writer.export_image(texture).unwrap();
            let writer = writer.compile().unwrap();
            let mut update = runtime.submit(&writer).unwrap();
            runtime.delete(old_uniform.key()).unwrap();
            runtime.delete(old_texture.key()).unwrap();
            assert!(runtime.refresh_buffer(old_uniform.key()).is_err());
            assert!(runtime.refresh_image(old_texture.key()).is_err());
            drop((plan, writer));
            let mut result = pending.wait(WAIT).unwrap().unwrap();
            let mut actual = [0; fixture::IMAGE_BYTES];
            result.readback_into(pass, &mut actual).unwrap();
            assert_eq!(
                actual,
                compute_fixture::image_bytes(&[fixture::final_color(2); fixture::PIXELS])
            );
            drop(result);
            drop(update.wait(WAIT).unwrap().unwrap());
            drop((pending, update, old_uniform, old_texture));
        }};
    }

    fn run(strict: bool) {
        let mut vk = VulkanRuntime::new_with_validation(
            RuntimeConfig {
                execution_slots: 2,
                resource_capacity: 128,
                max_shader_bindings: 64,
                ..Default::default()
            },
            if strict {
                ValidationMode::Required
            } else {
                ValidationMode::Disabled
            },
        )
        .expect("Vulkan hardware/validation prerequisites");
        let mut dx = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            capacity: D3D12FrameCapacity {
                executions: 2,
                buffers: 5,
                provenance_segments_per_resource: 16,
                ..Default::default()
            },
        })
        .expect("D3D12 hardware/validation prerequisites");
        let vk_info = vk.device_info();
        let dx_info = dx.device_info();
        assert_ne!(vk_info.device_type, "CPU", "hardware required");
        assert_eq!(
            vk_info.device_luid,
            Some(dx_info.luid),
            "same adapter required"
        );
        let vk_monitor = vk.validation_monitor();
        let dx_monitor = dx.validation_monitor();
        prepare_vk(&mut vk);
        prepare_dx(&mut dx);
        assert_eq!(
            vk.prepare_raster_programs(&fixture::too_many_programs())
                .unwrap_err()
                .kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(
            dx.prepare_graph_working_set(&images(), &fixture::too_many_programs(), &[])
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::CapacityExhausted
        );
        let mut pool = GraphPool::new(fixture::capacity(), 4).unwrap();
        let mut frames = Vec::new();
        for frame in 0..fixture::CASES * 2 {
            let (graph, expected) = fixture::build(&mut pool, frame);
            let plan = graph.compile().unwrap();
            let mut pending = vk.submit(&plan).unwrap();
            let mut result = pending.wait(WAIT).unwrap().expect("Vulkan completion");
            let vk_bytes = compare(&expected, |pass, output| {
                result.readback_into(pass, output).unwrap()
            });
            drop((result, pending));
            let mut pending = dx.submit(&plan).unwrap();
            let mut result = pending.wait(WAIT).unwrap().expect("D3D12 completion");
            let dx_bytes = compare(&expected, |pass, output| {
                result.readback_into(pass, output).unwrap()
            });
            assert_eq!(
                vk_bytes, dx_bytes,
                "same Graph/input API comparison frame {frame}"
            );
            frames
                .push(json!({"frame":frame,"bytes":vk_bytes.len(),"fnv1a64":checksum(&vk_bytes)}));
            drop((result, pending, plan));
        }
        pending_input_updates!(vk, pool);
        pending_input_updates!(dx, pool);
        let (graph, expected) = fixture::build(&mut pool, 2);
        let plan: CompiledGraph = graph.compile().unwrap();
        let recording_a = dx.prepare(&plan).unwrap();
        let recording_b = dx.prepare(&plan).unwrap();
        assert_eq!(
            dx.prepare(&plan).err().unwrap().kind(),
            D3D12RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(
            dx.prepare_graph_working_set(&images(), &[ManagedRasterProgram::mesh_hdr()], &[])
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::CapacityExhausted
        );
        drop((recording_a, recording_b));
        let mut pending = vk.submit(&plan).unwrap();
        assert_eq!(
            vk.prepare_raster_programs(&[ManagedRasterProgram::mesh_hdr()])
                .unwrap_err()
                .kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        let mut result = pending.wait(WAIT).unwrap().unwrap();
        compare(&expected, |pass, output| {
            result.readback_into(pass, output).unwrap()
        });
        assert_eq!(
            vk.prepare_raster_programs(&[ManagedRasterProgram::mesh_hdr()])
                .unwrap_err()
                .kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        drop((result, pending));
        let mut pending = dx.submit(&plan).unwrap();
        assert_eq!(
            dx.prepare_graph_working_set(&images(), &[ManagedRasterProgram::mesh_hdr()], &[])
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::CapacityExhausted
        );
        let mut result = pending.wait(WAIT).unwrap().unwrap();
        compare(&expected, |pass, output| {
            result.readback_into(pass, output).unwrap()
        });
        assert_eq!(
            dx.prepare_graph_working_set(&images(), &[ManagedRasterProgram::mesh_hdr()], &[])
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::CapacityExhausted
        );
        drop((result, pending));

        // A static descriptor is immutable. Replace the complete prepared set,
        // then prove the old CPU plan cannot silently resolve to another asset.
        vk.prepare_raster_programs(&[ManagedRasterProgram::mesh_hdr()])
            .unwrap();
        dx.prepare_graph_working_set(&images(), &[ManagedRasterProgram::mesh_hdr()], &[])
            .unwrap();
        assert!(
            vk.submit(&plan).is_err(),
            "missing custom asset is unprepared"
        );
        assert!(
            dx.prepare(&plan).is_err(),
            "missing custom asset is unprepared"
        );
        prepare_vk(&mut vk);
        prepare_dx(&mut dx);
        drop(plan);
        let (graph, vk_expected) = fixture::build(&mut pool, 2);
        let plan = graph.compile().unwrap();
        let mut pending = vk.submit(&plan).unwrap();
        drop(plan);
        drop(vk);
        let mut result = pending.wait(WAIT).unwrap().unwrap();
        let escaped_vk = compare(&vk_expected, |pass, output| {
            result.readback_into(pass, output).unwrap()
        });
        drop((result, pending));
        let (graph, dx_expected) = fixture::build(&mut pool, 2);
        let plan = graph.compile().unwrap();
        let mut pending = dx.submit(&plan).unwrap();
        drop(plan);
        dx.shutdown().unwrap();
        let mut result = pending.wait(WAIT).unwrap().unwrap();
        let escaped_dx = compare(&dx_expected, |pass, output| {
            result.readback_into(pass, output).unwrap()
        });
        assert_eq!(escaped_vk, escaped_dx);
        drop((result, pending, pool));
        let vk_report = vk_monitor.report();
        let dx_report = dx_monitor.snapshot();
        let report = json!({
            "scope":"consumer-custom-raster", "strict":strict,
            "configuration":{"development_validation":cfg!(feature="development-validation"),"graph_diagnostics":cfg!(feature="graph-diagnostics"),"vulkan_diagnostics":cfg!(feature="vulkan-diagnostics"),"d3d12_diagnostics":cfg!(feature="d3d12-diagnostics"),"debug_assertions":cfg!(debug_assertions)},
            "adapter":{"name":vk_info.name,"luid":dx_info.luid,"vendor_id":dx_info.vendor_id,"device_id":dx_info.device_id,"vulkan_driver":vk_info.driver_info,"d3d12_driver":dx_info.driver_version},
            "routes":["standard recipe direct","semantic post custom Raster","standard opaque + custom Raster + standard output copy"],
            "vertex_stride":24,"inline_bytes":32,"inline_stages":["vertex","fragment"],"sparse_binding_groups":[0,1,2],"frames":frames,
            "depth_unchanged":true,"record_cancel_capacity_reuse":true,"pending_and_result_working_set_gate":true,"complete_program_set_replacement":true,"pending_input_update_and_delete":true,"escaped_program_and_resource_owners":true,
            "validation":{"vulkan":{"requested":format!("{:?}",vk_report.requested),"core":vk_report.core_validation,"sync":vk_report.synchronization_validation,"warnings":vk_report.warnings,"errors":vk_report.errors,"callback_failures":vk_report.callback_failures,"dropped_problems":vk_report.dropped_problems,"problems":vk_report.problems},"d3d12":{"debug_layer":dx_report.debug_layer_enabled,"gbv":dx_report.gpu_based_validation_enabled,"teardown_checked":dx_report.teardown_checked,"warnings":dx_report.warnings,"errors":dx_report.errors,"corruption":dx_report.corruption,"callback_failures":dx_report.callback_failures,"dropped_messages":dx_report.dropped_messages}}
        });
        eprintln!("CUSTOM_RASTER_REPORT={report}");
        assert!(vk_report.is_clean(), "Vulkan diagnostics: {report}");
        assert!(dx_report.is_clean(), "D3D12 diagnostics: {report}");
        if strict {
            assert_eq!(vk_report.core_validation, Some(true));
            assert_eq!(vk_report.synchronization_validation, Some(true));
            assert!(
                dx_report.debug_layer_enabled
                    && dx_report.gpu_based_validation_enabled
                    && dx_report.teardown_checked
            );
        } else {
            assert_eq!(vk_report.requested, ValidationMode::Disabled);
            assert!(!dx_report.debug_layer_enabled && !dx_report.gpu_based_validation_enabled);
        }
    }
    #[cfg(all(feature = "vulkan-diagnostics", feature = "d3d12-diagnostics"))]
    #[test]
    #[ignore = "requires same-adapter Vulkan/D3D12 hardware and strict native validation"]
    fn custom_raster_common_graph_strict() {
        run(true);
    }

    #[cfg(not(any(
        feature = "development-validation",
        feature = "vulkan-diagnostics",
        feature = "d3d12-diagnostics",
        feature = "graph-diagnostics"
    )))]
    #[test]
    #[ignore = "requires same-adapter Vulkan/D3D12 hardware with diagnostics excluded"]
    fn custom_raster_common_graph_shipping() {
        assert!(
            !cfg!(debug_assertions),
            "shipping acceptance requires --release"
        );
        run(false);
    }
}

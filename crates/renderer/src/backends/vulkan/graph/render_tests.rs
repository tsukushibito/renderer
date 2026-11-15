//! Real-GPU shader, depth and complete M2 integration contracts.

use super::tests::prepare_scenario;
use super::*;
use crate::graph::{Attachment, LoadOp, RasterAttachments, RasterDraw, StoreOp};
use crate::graph::{
    BufferDesc, BufferHandle, ComputeTransform, Graph, ImageDesc, RasterParameters,
};

const WAIT: Duration = Duration::from_secs(10);

#[test]
#[ignore = "real GPU attachment lowering, cache rejection and culled subresources; run strict and shipping separately"]
fn vulkan_attachment_subresource_native_boundaries() {
    let strict = cfg!(feature = "vulkan-diagnostics");
    let mut runtime = VulkanRuntime::new_with_validation(
        RuntimeConfig::default(),
        if strict {
            super::super::ValidationMode::Required
        } else {
            super::super::ValidationMode::Disabled
        },
    )
    .unwrap();
    assert_ne!(
        runtime.owner.device.device_type(),
        vk::PhysicalDeviceType::CPU
    );
    let monitor = runtime.validation_monitor();
    {
        let runtime = &mut runtime;
        let color_desc = ImageDesc::rgba8(4, 4);
        let depth_desc = ImageDesc::depth32(4, 4);
        prepare_scenario(
            runtime,
            6,
            8,
            &[
                ResourceClass::Image {
                    desc: color_desc,
                    count: 2,
                },
                ResourceClass::Image {
                    desc: depth_desc,
                    count: 1,
                },
            ],
        );
        // Both descriptors would hit an existing base-extent cache key without
        // the capability guard. Rejection must precede selection/reuse.
        let before = runtime.stats();
        for desc in [
            ImageDesc {
                mip_levels: 2,
                ..color_desc
            },
            ImageDesc {
                array_layers: 2,
                ..color_desc
            },
        ] {
            assert_eq!(
                runtime
                    .acquire_inner(ResourceDesc::Image(desc), "unsupported cache lookup", true)
                    .err()
                    .unwrap()
                    .kind(),
                RuntimeErrorKind::Unsupported
            );
            assert_eq!(
                runtime
                    .prepare_working_set(
                        Default::default(),
                        &[ResourceClass::Image { desc, count: 1 }]
                    )
                    .unwrap_err()
                    .kind(),
                RuntimeErrorKind::Unsupported
            );
        }
        assert_eq!(runtime.stats().resources_created, before.resources_created);
        assert_eq!(runtime.stats().resources_reused, before.resources_reused);

        for mode in 0..3 {
            let mut graph = Graph::test_new();
            let color = graph.image("color", color_desc).unwrap();
            let second = graph.image("second", color_desc).unwrap();
            let depth = graph.image("depth", depth_desc).unwrap();
            let colors = [color, second].map(|image| Attachment {
                view: graph.image_view(image, 0, 0).unwrap(),
                load: LoadOp::Clear([1.0; 4]),
                store: StoreOp::Store,
            });
            let depth_attachment = Attachment {
                view: graph.image_view(depth, 0, 0).unwrap(),
                load: LoadOp::Clear(1.0),
                store: StoreOp::Store,
            };
            graph
                .attachment_pass(
                    "unsupported shape",
                    RasterAttachments {
                        colors: &colors[..[1, 0, 2][mode]],
                        depth: (mode == 1).then_some(depth_attachment),
                    },
                )
                .unwrap();
            if mode == 1 {
                graph.export_image(depth).unwrap();
            } else {
                graph.export_image(color).unwrap();
            }
            assert_eq!(
                runtime
                    .submit(&graph.compile().unwrap())
                    .err()
                    .unwrap()
                    .kind(),
                RuntimeErrorKind::Unsupported
            );
        }

        let mut graph = Graph::test_new();
        let color = graph.image("clear color", color_desc).unwrap();
        let depth = graph.image("clear depth", depth_desc).unwrap();
        graph
            .attachment_pass(
                "supported clear",
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(color, 0, 0).unwrap(),
                        load: LoadOp::Clear([1.0, 0.0, 0.0, 1.0]),
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
        let read = graph.readback_image("color bytes", color).unwrap();
        let multi = graph
            .image(
                "dead multi image",
                ImageDesc {
                    mip_levels: 2,
                    array_layers: 2,
                    ..color_desc
                },
            )
            .unwrap();
        graph
            .attachment_pass(
                "dead subresource",
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(multi, 1, 1).unwrap(),
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    depth: None,
                },
            )
            .unwrap();
        let mut submission = runtime.submit(&graph.compile().unwrap()).unwrap();
        let mut result = submission.wait(WAIT).unwrap().unwrap();
        let bytes = result.test_readback(read).unwrap();
        assert_eq!(bytes.len(), 64);
        assert!(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [255, 0, 0, 255])
        );
    }
    drop(runtime);
    let report = monitor.report();
    assert!(report.is_clean(), "teardown diagnostics: {report:?}");
    if strict {
        assert_eq!(report.core_validation, Some(true));
        assert_eq!(report.synchronization_validation, Some(true));
    } else {
        assert_eq!(report.requested, super::super::ValidationMode::Disabled);
        assert_eq!(report.core_validation, None);
        assert_eq!(report.synchronization_validation, None);
    }
}

fn batch_draws(draws: Vec<(BufferHandle, BufferHandle, u32, RasterParameters)>) -> Vec<RasterDraw> {
    draws
        .into_iter()
        .map(|(vertices, indices, index_count, parameters)| RasterDraw {
            vertices,
            indices,
            index_count,
            parameters,
        })
        .collect()
}

fn real_gpu(test: impl FnOnce(&mut VulkanRuntime)) {
    real_gpu_config(RuntimeConfig::default(), test);
}

fn real_gpu_config(config: RuntimeConfig, test: impl FnOnce(&mut VulkanRuntime)) {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(config).expect("real Vulkan runtime is required");
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU,
            "CPU fallback does not count as real-GPU acceptance"
        );
        let diagnostics = runtime.diagnostics.clone();
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        test(&mut runtime);
        drop(runtime);
        let report = diagnostics.snapshot();
        assert!(
            report.is_clean(),
            "Core/SyncVal and callback failures, including shutdown: {report:#?}"
        );
    });
}

fn transform(bytes: &[u8], operation: ComputeTransform) -> Vec<u8> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| match operation {
            ComputeTransform::Invert => [255 - p[0], 255 - p[1], 255 - p[2], p[3]],
            ComputeTransform::SwapRedBlue => [p[2], p[1], p[0], p[3]],
        })
        .collect()
}

/// Scalar reference and opaque batch encode exactly the same owned draws.
fn opaque_batch_comparison(
    batched: bool,
    reverse: bool,
    empty: bool,
    invalid_last_indices: bool,
) -> (CompiledGraph, PassHandle) {
    let mut graph = Graph::test_new();
    let mut upload_mesh = |name: &str, z: f32, invalid: bool| {
        let bytes = [[-0.4_f32, -0.4, z], [0.4, -0.4, z], [0.0, 0.4, z]]
            .into_iter()
            .flat_map(|position| position.into_iter().chain([1.0; 4]))
            .flat_map(f32::to_ne_bytes)
            .collect::<Vec<_>>();
        let vertices = graph
            .buffer(&format!("{name} vertices"), BufferDesc { size: 84 })
            .unwrap();
        graph
            .upload_buffer(&format!("{name} vertices upload"), vertices, 0, &bytes)
            .unwrap();
        let source = graph
            .buffer(&format!("{name} index source"), BufferDesc { size: 12 })
            .unwrap();
        let indices = graph
            .buffer(&format!("{name} indices"), BufferDesc { size: 12 })
            .unwrap();
        let bytes: Vec<u8> = [0_u32, 1, if invalid { 9 } else { 2 }]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        graph
            .upload_buffer(&format!("{name} index upload"), source, 0, &bytes)
            .unwrap();
        graph
            .copy_buffer(&format!("{name} index copy"), source, 0, indices, 0, 12)
            .unwrap();
        (vertices, indices)
    };
    let near = upload_mesh("near", 0.25, false);
    let far = upload_mesh("far", 0.75, invalid_last_indices);
    let color = graph.image("color", ImageDesc::rgba8(64, 64)).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(64, 64)).unwrap();
    let parameters = |tint| RasterParameters {
        tint,
        ..RasterParameters::default()
    };
    let mut shifted = parameters([1.0, 1.0, 0.0, 1.0]);
    shifted.transform[3][0] = 0.75;
    let mut draws = vec![
        (near.0, near.1, 3, parameters([1.0, 0.0, 0.0, 1.0])),
        (far.0, far.1, 3, parameters([0.0, 1.0, 0.0, 1.0])),
        (near.0, near.1, 3, parameters([0.0, 0.0, 1.0, 1.0])),
        (near.0, near.1, 3, shifted),
    ];
    if empty {
        draws.clear();
    }
    if reverse {
        draws.reverse();
    }
    if batched {
        graph
            .raster(
                "opaque batch",
                &crate::program::ManagedRasterProgram::mesh(),
                &batch_draws(draws),
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(color, 0, 0).unwrap(),
                        load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
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
    } else {
        graph
            .attachment_pass(
                "opaque clear",
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(color, 0, 0).unwrap(),
                        load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
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
        for (vertices, indices, count, parameters) in draws {
            graph
                .raster(
                    "scalar draw",
                    &crate::program::ManagedRasterProgram::mesh(),
                    &[RasterDraw {
                        vertices,
                        indices,
                        index_count: count,
                        parameters,
                    }],
                    RasterAttachments {
                        colors: &[Attachment {
                            view: graph.image_view(color, 0, 0).unwrap(),
                            load: LoadOp::Load,
                            store: StoreOp::Store,
                        }],
                        depth: Some(Attachment {
                            view: graph.image_view(depth, 0, 0).unwrap(),
                            load: LoadOp::Load,
                            store: StoreOp::Store,
                        }),
                    },
                )
                .unwrap();
        }
    }
    let inverted = graph.image("inverted", ImageDesc::rgba8(64, 64)).unwrap();
    graph
        .compute(
            "invert after opaque",
            color,
            inverted,
            ComputeTransform::Invert,
        )
        .unwrap();
    let output = graph.image("composite", ImageDesc::rgba8(64, 64)).unwrap();
    graph
        .composite("composite after compute", inverted, output)
        .unwrap();
    let readback = graph.readback_image("output", output).unwrap();
    (graph.compile().unwrap(), readback)
}

fn prepare_opaque_scenario(runtime: &mut VulkanRuntime, additional: &[ResourceClass]) {
    let mut classes = vec![
        ResourceClass::Buffer {
            desc: BufferDesc { size: 84 },
            count: 2,
        },
        ResourceClass::Buffer {
            desc: BufferDesc { size: 12 },
            count: 4,
        },
        ResourceClass::Image {
            desc: ImageDesc::rgba8(64, 64),
            count: 3,
        },
        ResourceClass::Image {
            desc: ImageDesc::depth32(64, 64),
            count: 1,
        },
    ];
    classes.extend_from_slice(additional);
    prepare_scenario(runtime, 10, 14, &classes);
}

#[test]
#[ignore = "real-GPU opaque batch/scalar reference; requires Core/SyncVal"]
fn vulkan_graph_batch_matches_scalar_draw_order_depth_parameters_and_compute() {
    real_gpu(|runtime| {
        prepare_opaque_scenario(runtime, &[]);
        for empty in [false, true] {
            for reverse in [false, true] {
                let mut reference = None;
                for batched in [false, true] {
                    let (plan, pass) = opaque_batch_comparison(batched, reverse, empty, false);
                    let mut submission = runtime.submit(&plan).unwrap();
                    let mut result = submission.wait(WAIT).unwrap().unwrap();
                    let bytes = result.test_readback(pass).unwrap();
                    assert_eq!(&bytes[..4], &[255, 255, 255, 255]);
                    let center = (32 * 64 + 32) * 4;
                    let expected = if empty {
                        [255; 4]
                    } else if reverse {
                        [255, 255, 0, 255]
                    } else {
                        [0, 255, 255, 255]
                    };
                    assert_eq!(&bytes[center..center + 4], &expected);
                    if let Some(reference) = &reference {
                        assert_eq!(&bytes, reference);
                    } else {
                        reference = Some(bytes);
                    }
                }
            }
        }
    });
}

#[test]
#[ignore = "real-GPU batch late index validation; requires Core/SyncVal"]
fn vulkan_graph_batch_rejects_later_draw_invalid_indices_before_submission() {
    real_gpu(|runtime| {
        prepare_opaque_scenario(
            runtime,
            &[
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 28 },
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(1, 1),
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::depth32(1, 1),
                    count: 1,
                },
            ],
        );
        let (plan, _) = opaque_batch_comparison(true, false, false, true);
        let error = match runtime.submit(&plan) {
            Ok(_) => panic!("later draw indices must be checked"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), RuntimeErrorKind::InvalidInput);
        assert!(error.to_string().contains("index 9"));
        assert!(error.to_string().contains("draw 1"));

        // One index buffer can be legal for the first draw but illegal for the
        // next draw's smaller vertex extent. Reuse must include those bounds.
        let mut graph = Graph::test_new();
        let full = graph
            .buffer("three vertices", BufferDesc { size: 84 })
            .unwrap();
        let short = graph.buffer("one vertex", BufferDesc { size: 28 }).unwrap();
        let indices = graph
            .buffer("shared indices", BufferDesc { size: 12 })
            .unwrap();
        graph
            .upload_buffer("full upload", full, 0, &[0; 84])
            .unwrap();
        graph
            .upload_buffer("short upload", short, 0, &[0; 28])
            .unwrap();
        graph
            .upload_buffer(
                "index upload",
                indices,
                0,
                &[0_u32, 1, 2]
                    .into_iter()
                    .flat_map(u32::to_ne_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let color = graph.image("color", ImageDesc::rgba8(1, 1)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(1, 1)).unwrap();
        graph
            .raster(
                "bounds batch",
                &crate::program::ManagedRasterProgram::mesh(),
                &batch_draws(vec![
                    (full, indices, 3, RasterParameters::default()),
                    (short, indices, 3, RasterParameters::default()),
                ]),
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(color, 0, 0).unwrap(),
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
        graph.readback_image("read", color).unwrap();
        let error = match runtime.submit(&graph.compile().unwrap()) {
            Ok(_) => panic!("each draw needs its own vertex bounds"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), RuntimeErrorKind::InvalidInput);
        assert!(error.to_string().contains("vertex count 1"));
        assert!(error.to_string().contains("draw 1"));
        let (plan, pass) = opaque_batch_comparison(true, false, false, false);
        let mut submission = runtime.submit(&plan).unwrap();
        let mut result = submission.wait(WAIT).unwrap().unwrap();
        assert_eq!(result.test_readback(pass).unwrap().len(), 64 * 64 * 4);
    });
}

#[test]
#[ignore = "real-GPU shared vertex/index buffer in an opaque batch; requires Core/SyncVal"]
fn vulkan_graph_batch_unions_shared_vertex_and_index_scopes() {
    real_gpu(|runtime| {
        prepare_scenario(
            runtime,
            3,
            3,
            &[
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 84 },
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(4, 4),
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::depth32(4, 4),
                    count: 1,
                },
            ],
        );
        let mut graph = Graph::test_new();
        let buffer = graph
            .buffer("shared input", BufferDesc { size: 84 })
            .unwrap();
        // Zero index values address the first vertex. The degenerate triangles
        // leave the clear intact while exercising both native read purposes.
        graph
            .upload_buffer("shared upload", buffer, 0, &[0; 84])
            .unwrap();
        let color = graph.image("color", ImageDesc::rgba8(4, 4)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(4, 4)).unwrap();
        graph
            .raster(
                "shared input batch",
                &crate::program::ManagedRasterProgram::mesh(),
                &batch_draws(vec![(buffer, buffer, 3, RasterParameters::default()); 3]),
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(color, 0, 0).unwrap(),
                        load: LoadOp::Clear([0.0, 0.0, 1.0, 1.0]),
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
        let readback = graph.readback_image("read", color).unwrap();
        let compiled = graph.compile().unwrap();
        #[cfg(feature = "graph-diagnostics")]
        let (mut submission, lowering) = runtime.submit_with_diagnostics(&compiled).unwrap();
        #[cfg(not(feature = "graph-diagnostics"))]
        let mut submission = runtime.submit(&compiled).unwrap();
        #[cfg(feature = "graph-diagnostics")]
        assert!(
            lowering.lines().any(|line| {
                line.trim_start().starts_with("buffer=")
                    && line.contains("VERTEX_ATTRIBUTE_READ")
                    && line.contains("INDEX_READ")
            }),
            "{lowering}"
        );
        let mut result = submission.wait(WAIT).unwrap().unwrap();
        assert_eq!(
            result.test_readback(readback).unwrap(),
            [0, 0, 255, 255].repeat(16)
        );
    });
}

#[test]
#[ignore = "explicit real-GPU descriptor failure cleanup; requires Core/SyncVal"]
fn vulkan_graph_descriptor_exhaustion_releases_unsubmitted_leases() {
    real_gpu_config(
        RuntimeConfig {
            max_shader_bindings: 1,
            staging_ranges: 256,
            ..RuntimeConfig::default()
        },
        |runtime| {
            prepare_scenario(
                runtime,
                4,
                5,
                &[
                    ResourceClass::Image {
                        desc: ImageDesc::rgba8(8, 8),
                        count: 3,
                    },
                    ResourceClass::Buffer {
                        desc: BufferDesc { size: 224 },
                        count: 1,
                    },
                    ResourceClass::Buffer {
                        desc: BufferDesc { size: 48 },
                        count: 1,
                    },
                    ResourceClass::Image {
                        desc: ImageDesc::rgba8(16, 16),
                        count: 1,
                    },
                    ResourceClass::Image {
                        desc: ImageDesc::depth32(16, 16),
                        count: 1,
                    },
                ],
            );
            let mut graph = Graph::test_new();
            let source = graph
                .image("capacity-source", ImageDesc::rgba8(8, 8))
                .unwrap();
            let first = graph
                .image("capacity-first", ImageDesc::rgba8(8, 8))
                .unwrap();
            let second = graph
                .image("capacity-second", ImageDesc::rgba8(8, 8))
                .unwrap();
            let bytes = [13, 29, 87, 173].repeat(64);
            graph
                .upload_image("capacity-upload", source, &bytes)
                .unwrap();
            graph
                .compute("first-binding", source, first, ComputeTransform::Invert)
                .unwrap();
            graph
                .compute("exhausted-binding", first, second, ComputeTransform::Invert)
                .unwrap();
            graph.readback_image("capacity-output", second).unwrap();
            let error = match runtime.submit(&graph.compile().unwrap()) {
                Ok(_) => panic!("descriptor capacity must fail before submission"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), RuntimeErrorKind::CapacityExhausted);
            let failed = runtime.stats();
            assert_eq!(
                (failed.upload_live_ranges, failed.readback_live_ranges),
                (0, 0)
            );
            assert_eq!(failed.registered_resources, 0);
            // A smaller graph reuses the canceled arena, images and staging without
            // accidentally publishing the rejected graph's initialized contents.
            let mut graph = Graph::test_new();
            let source = graph
                .image("recovered-source", ImageDesc::rgba8(8, 8))
                .unwrap();
            let destination = graph
                .image("recovered-destination", ImageDesc::rgba8(8, 8))
                .unwrap();
            graph
                .upload_image("recovered-upload", source, &bytes)
                .unwrap();
            graph
                .compute(
                    "recovered-compute",
                    source,
                    destination,
                    ComputeTransform::Invert,
                )
                .unwrap();
            let read = graph.readback_image("recovered-read", destination).unwrap();
            let mut pending = runtime.submit(&graph.compile().unwrap()).unwrap();
            let mut result = pending
                .wait(WAIT)
                .unwrap()
                .expect("recovered graph must complete");
            assert_eq!(
                result.test_readback(read).unwrap(),
                transform(&bytes, ComputeTransform::Invert)
            );
            let recovered = runtime.stats();
            assert_eq!(
                recovered.descriptor_arenas_created,
                runtime.config.execution_slots
            );
            assert!(recovered.descriptor_arenas_reused >= 1);
            assert_eq!(
                (recovered.upload_live_ranges, recovered.readback_live_ranges),
                (0, 0)
            );
            // Byte extent validation alone cannot establish safe indexed access.
            // A partial upload that changes one actual index must be rejected while
            // still unsubmitted, with all acquired leases canceled.
            let mut invalid = Graph::test_new();
            let (vertices, indices) = add_mesh(&mut invalid, false);
            invalid
                .upload_buffer("invalid-first-index", indices, 0, &999u32.to_le_bytes())
                .unwrap();
            let color = invalid
                .image("invalid-index-color", ImageDesc::rgba8(16, 16))
                .unwrap();
            let depth = invalid
                .image("invalid-index-depth", ImageDesc::depth32(16, 16))
                .unwrap();
            invalid
                .raster(
                    "reject-invalid-index",
                    &crate::program::ManagedRasterProgram::mesh(),
                    &[RasterDraw {
                        vertices,
                        indices,
                        index_count: 12,
                        parameters: RasterParameters::default(),
                    }],
                    RasterAttachments {
                        colors: &[Attachment {
                            view: invalid.image_view(color, 0, 0).unwrap(),
                            load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
                            store: StoreOp::Store,
                        }],
                        depth: Some(Attachment {
                            view: invalid.image_view(depth, 0, 0).unwrap(),
                            load: LoadOp::Clear(1.0),
                            store: StoreOp::Store,
                        }),
                    },
                )
                .unwrap();
            invalid.readback_image("invalid-index-read", color).unwrap();
            let error = match runtime.submit(&invalid.compile().unwrap()) {
                Ok(_) => panic!("out-of-range actual index must be rejected before submit"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), RuntimeErrorKind::InvalidInput);
            assert!(error.message.contains("index 999"), "{error}");
            assert!(error.message.contains("vertex count 8"), "{error}");
            let rejected = runtime.stats();
            assert_eq!(
                (rejected.upload_live_ranges, rejected.readback_live_ranges),
                (0, 0)
            );
            assert_eq!(rejected.registered_resources, 0);
        },
    );
}

#[test]
#[ignore = "explicit real-GPU image Compute dependencies; requires Core/SyncVal"]
fn vulkan_graph_dependent_compute_and_descriptor_reuse() {
    real_gpu(|runtime| {
        prepare_scenario(
            runtime,
            3,
            5,
            &[ResourceClass::Image {
                desc: ImageDesc::rgba8(17, 13),
                count: 3,
            }],
        );
        let mut graph = Graph::test_new();
        let desc = ImageDesc::rgba8(17, 13); // dispatch tail requires shader bounds guard
        let source = graph.image("compute-source", desc).unwrap();
        let first = graph.image("compute-inverted", desc).unwrap();
        let second = graph.image("compute-swizzled", desc).unwrap();
        let bytes: Vec<u8> = (0..17 * 13)
            .flat_map(|pixel| {
                [
                    (pixel * 17) as u8,
                    (pixel * 31 + 1) as u8,
                    (pixel * 43 + 2) as u8,
                    (pixel * 7 + 3) as u8,
                ]
            })
            .collect();
        graph.upload_image("upload", source, &bytes).unwrap();
        graph
            .compute("invert", source, first, ComputeTransform::Invert)
            .unwrap();
        graph
            .compute(
                "swizzle-dependent",
                first,
                second,
                ComputeTransform::SwapRedBlue,
            )
            .unwrap();
        let first_read = graph.readback_image("read-inverted", first).unwrap();
        let second_read = graph.readback_image("read-swizzled", second).unwrap();
        let compiled = graph.compile().unwrap();
        let expected_first = transform(&bytes, ComputeTransform::Invert);
        let expected_second = transform(&expected_first, ComputeTransform::SwapRedBlue);
        let prepared_programs = runtime.programs.as_ref().unwrap().handles();
        assert_ne!(prepared_programs[0], vk::Pipeline::null());
        assert_eq!(prepared_programs[1], vk::Pipeline::null());
        assert_ne!(prepared_programs[2], vk::Pipeline::null());
        for _ in 0..4 {
            let mut submission = runtime.submit(&compiled).unwrap();
            assert_eq!(runtime.execution.stats().leased, 1);
            let handles = runtime.programs.as_ref().unwrap().handles();
            assert_eq!(handles, prepared_programs);
            let mut result = submission
                .wait(WAIT)
                .unwrap()
                .expect("Compute must complete");
            assert_eq!(result.test_readback(first_read).unwrap(), expected_first);
            assert_eq!(result.test_readback(second_read).unwrap(), expected_second);
        }
        let stats = runtime.stats();
        assert_eq!(stats.resources_created, 3, "images/views must be reused");
        assert!(stats.resources_reused >= 9);
        assert_eq!(
            stats.execution_slots_created,
            runtime.execution.stats().capacity,
            "all native execution slots are prepared before graph submission"
        );
        assert!(stats.execution_slots_reused >= 3);
        assert_eq!(
            stats.descriptor_arenas_created,
            runtime.config.execution_slots
        );
        assert!(stats.descriptor_arenas_reused >= 3);
        assert_eq!(
            (stats.upload_live_ranges, stats.readback_live_ranges),
            (0, 0)
        );
    });
}

#[test]
#[ignore = "explicit real-GPU prepared program selection with pending work; requires Core/SyncVal"]
fn vulkan_graph_prepared_program_kinds_survive_pending_work() {
    real_gpu(|runtime| {
        prepare_scenario(
            runtime,
            4,
            4,
            &[
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(4, 4),
                    count: 3,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 224 },
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 48 },
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(16, 16),
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::depth32(16, 16),
                    count: 1,
                },
            ],
        );
        let prepared_programs = runtime.programs.as_ref().unwrap().handles();
        assert!(
            prepared_programs
                .iter()
                .all(|&handle| handle != vk::Pipeline::null())
        );
        let prepared_resources = runtime.stats().resources_created;
        let mut producer = Graph::test_new();
        let source = producer
            .image("lazy-source", ImageDesc::rgba8(4, 4))
            .unwrap();
        let output = producer
            .image("lazy-computed", ImageDesc::rgba8(4, 4))
            .unwrap();
        let bytes = [13, 29, 87, 173].repeat(16);
        producer
            .upload_image("lazy-upload", source, &bytes)
            .unwrap();
        producer
            .compute("lazy-compute", source, output, ComputeTransform::Invert)
            .unwrap();
        producer.export_image(output).unwrap();
        let mut pending_compute = runtime.submit(&producer.compile().unwrap()).unwrap();
        let exported = pending_compute.export_image(output).unwrap();
        let first = runtime.programs.as_ref().unwrap().handles();
        assert_eq!(first, prepared_programs);
        assert_eq!(
            runtime
                .prepare_working_set(FrameCapacity::default(), &[])
                .unwrap_err()
                .kind(),
            RuntimeErrorKind::CapacityExhausted,
            "pending work prevents replacing the prepared backing"
        );

        // Select composite while the Compute submission still owns its program.
        let mut consumer = Graph::test_new();
        let source = consumer.import_image("lazy-import", &exported).unwrap();
        let output = consumer
            .image("lazy-composite", ImageDesc::rgba8(4, 4))
            .unwrap();
        consumer.composite("lazy-compose", source, output).unwrap();
        let read = consumer
            .readback_image("lazy-read-composite", output)
            .unwrap();
        let mut pending_composite = runtime.submit(&consumer.compile().unwrap()).unwrap();
        let second = runtime.programs.as_ref().unwrap().handles();
        assert_eq!(second, prepared_programs);

        // Select mesh without replacing either earlier program. All three kinds
        // coexist while the prior submissions still retain their leases.
        let mut mesh = Graph::test_new();
        let (vertices, indices) = add_mesh(&mut mesh, false);
        let color = mesh.image("lazy-color", ImageDesc::rgba8(16, 16)).unwrap();
        let depth = mesh
            .image("lazy-depth", ImageDesc::depth32(16, 16))
            .unwrap();
        mesh.raster(
            "lazy-mesh",
            &crate::program::ManagedRasterProgram::mesh(),
            &[RasterDraw {
                vertices,
                indices,
                index_count: 12,
                parameters: RasterParameters::default(),
            }],
            RasterAttachments {
                colors: &[Attachment {
                    view: mesh.image_view(color, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: mesh.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
        let mesh_read = mesh.readback_image("lazy-read-mesh", color).unwrap();
        let mut pending_mesh = runtime.submit(&mesh.compile().unwrap()).unwrap();
        let third = runtime.programs.as_ref().unwrap().handles();
        assert_eq!(third, prepared_programs);
        assert_eq!(runtime.stats().resources_created, prepared_resources);
        let mut mesh_result = pending_mesh.wait(WAIT).unwrap().expect("mesh must finish");
        assert_eq!(
            mesh_result.test_readback(mesh_read).unwrap(),
            expected_mesh([0; 4])
        );
        let mut composite_result = pending_composite
            .wait(WAIT)
            .unwrap()
            .expect("composite must finish");
        assert_eq!(
            composite_result.test_readback(read).unwrap(),
            transform(&bytes, ComputeTransform::Invert)
        );
        assert!(pending_compute.wait(WAIT).unwrap().is_some());
        runtime.delete(exported.key()).unwrap();
    });
}

fn vertex_bytes(vertices: &[[f32; 7]]) -> Vec<u8> {
    vertices
        .iter()
        .flatten()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn index_bytes(indices: &[u32]) -> Vec<u8> {
    indices
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// Integer pixel-aligned quads at16x16 avoid edge tolerance. Far red covers
/// [2,14)^2 at z=.75, near green covers [4,12)^2 at z=.25.
fn mesh(reverse: bool) -> (Vec<u8>, Vec<u8>) {
    let mut vertices = Vec::new();
    for (edge, z, color) in [
        (0.75, 0.75, [1.0, 0.0, 0.0, 1.0]),
        (0.5, 0.25, [0.0, 1.0, 0.0, 1.0]),
    ] {
        for (x, y) in [(-edge, -edge), (edge, -edge), (edge, edge), (-edge, edge)] {
            vertices.push([x, y, z, color[0], color[1], color[2], color[3]]);
        }
    }
    let first = [0, 1, 2, 2, 3, 0];
    let second = [4, 5, 6, 6, 7, 4];
    let indices = if reverse {
        [second, first].concat()
    } else {
        [first, second].concat()
    };
    (vertex_bytes(&vertices), index_bytes(&indices))
}

fn expected_mesh(background: [u8; 4]) -> Vec<u8> {
    (0..16)
        .flat_map(|y| {
            (0..16).flat_map(move |x| {
                if (4..12).contains(&x) && (4..12).contains(&y) {
                    [0, 255, 0, 255]
                } else if (2..14).contains(&x) && (2..14).contains(&y) {
                    [255, 0, 0, 255]
                } else {
                    background
                }
            })
        })
        .collect()
}

fn add_mesh(graph: &mut Graph, reverse: bool) -> (BufferHandle, BufferHandle) {
    let (vertices, indices) = mesh(reverse);
    let v = graph
        .buffer(
            "mesh-vertices",
            BufferDesc {
                size: vertices.len() as u64,
            },
        )
        .unwrap();
    let i = graph
        .buffer(
            "mesh-indices",
            BufferDesc {
                size: indices.len() as u64,
            },
        )
        .unwrap();
    graph
        .upload_buffer("upload-vertices", v, 0, &vertices)
        .unwrap();
    graph
        .upload_buffer("upload-indices", i, 0, &indices)
        .unwrap();
    (v, i)
}

#[test]
#[ignore = "explicit real-GPU indexed mesh/depth and attachment LOAD; requires Core/SyncVal"]
fn vulkan_graph_depth_occlusion_and_attachment_load() {
    real_gpu(|runtime| {
        prepare_scenario(
            runtime,
            6,
            10,
            &[
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 224 },
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 48 },
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 112 },
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 24 },
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(16, 16),
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::depth32(16, 16),
                    count: 1,
                },
            ],
        );
        for reverse in [false, true] {
            let mut graph = Graph::test_new();
            let (vertices, indices) = add_mesh(&mut graph, reverse);
            // The same upload writer must become visible to both COPY reads
            // and later VERTEX_ATTRIBUTE/INDEX reads, rather than replacing its
            // source scope with the first read's scope.
            let vertex_proof = graph
                .readback_buffer("read-uploaded-vertices", vertices, 0, 224)
                .unwrap();
            let index_proof = graph
                .readback_buffer("read-uploaded-indices", indices, 0, 48)
                .unwrap();
            let color = graph.image("mesh-color", ImageDesc::rgba8(16, 16)).unwrap();
            let depth = graph
                .image("mesh-depth", ImageDesc::depth32(16, 16))
                .unwrap();
            graph
                .raster(
                    "mesh-clear",
                    &crate::program::ManagedRasterProgram::mesh(),
                    &[RasterDraw {
                        vertices,
                        indices,
                        index_count: 12,
                        parameters: RasterParameters::default(),
                    }],
                    RasterAttachments {
                        colors: &[Attachment {
                            view: graph.image_view(color, 0, 0).unwrap(),
                            load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
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
            let initial = graph.readback_image("read-before-load", color).unwrap();
            let blue = vertex_bytes(&[
                [-1.0, -1.0, 0.9, 0.0, 0.0, 1.0, 1.0],
                [1.0, -1.0, 0.9, 0.0, 0.0, 1.0, 1.0],
                [1.0, 1.0, 0.9, 0.0, 0.0, 1.0, 1.0],
                [-1.0, 1.0, 0.9, 0.0, 0.0, 1.0, 1.0],
            ]);
            let blue_vertices = graph
                .buffer(
                    "background-vertices",
                    BufferDesc {
                        size: blue.len() as u64,
                    },
                )
                .unwrap();
            let blue_indices = graph
                .buffer("background-indices", BufferDesc { size: 24 })
                .unwrap();
            graph
                .upload_buffer("upload-background-vertices", blue_vertices, 0, &blue)
                .unwrap();
            graph
                .upload_buffer(
                    "upload-background-indices",
                    blue_indices,
                    0,
                    &index_bytes(&[0, 1, 2, 2, 3, 0]),
                )
                .unwrap();
            graph
                .raster(
                    "mesh-load",
                    &crate::program::ManagedRasterProgram::mesh(),
                    &[RasterDraw {
                        vertices: blue_vertices,
                        indices: blue_indices,
                        index_count: 6,
                        parameters: RasterParameters::default(),
                    }],
                    RasterAttachments {
                        colors: &[Attachment {
                            view: graph.image_view(color, 0, 0).unwrap(),
                            load: LoadOp::Load,
                            store: StoreOp::Store,
                        }],
                        depth: Some(Attachment {
                            view: graph.image_view(depth, 0, 0).unwrap(),
                            load: LoadOp::Load,
                            store: StoreOp::Store,
                        }),
                    },
                )
                .unwrap();
            let loaded = graph.readback_image("read-after-load", color).unwrap();
            let compiled = graph.compile().unwrap();
            let mut submission = runtime.submit(&compiled).unwrap();
            let mut result = submission.wait(WAIT).unwrap().expect("mesh must complete");
            let (expected_vertices, expected_indices) = mesh(reverse);
            assert_eq!(
                result.test_readback(vertex_proof).unwrap(),
                expected_vertices
            );
            assert_eq!(result.test_readback(index_proof).unwrap(), expected_indices);
            assert_eq!(
                result.test_readback(initial).unwrap(),
                expected_mesh([0, 0, 0, 255]),
                "depth must be independent of triangle order"
            );
            assert_eq!(
                result.test_readback(loaded).unwrap(),
                expected_mesh([0, 0, 255, 255]),
                "LOAD must preserve both color and depth across rendering scopes and a Copy read"
            );
        }
    });
}

#[test]
#[ignore = "explicit real-GPU full M2 flow and pending export handoff; requires Core/SyncVal"]
fn vulkan_graph_full_m2_composite_and_pending_handoff() {
    real_gpu(|runtime| {
        prepare_scenario(
            runtime,
            8,
            8,
            &[
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 224 },
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 48 },
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(16, 16),
                    count: 5,
                },
                ResourceClass::Image {
                    desc: ImageDesc::depth32(16, 16),
                    count: 1,
                },
            ],
        );
        let expected_scene = expected_mesh([0, 0, 0, 255]);
        let expected = transform(
            &transform(&expected_scene, ComputeTransform::Invert),
            ComputeTransform::SwapRedBlue,
        );
        let expected_consumer = transform(&expected, ComputeTransform::Invert);
        for reverse in [false, true] {
            let mut graph = Graph::test_new();
            let (vertices, indices) = add_mesh(&mut graph, reverse);
            let color = graph
                .image("scene-color", ImageDesc::rgba8(16, 16))
                .unwrap();
            let depth = graph
                .image("scene-depth", ImageDesc::depth32(16, 16))
                .unwrap();
            let inverted = graph
                .image("inverted-color", ImageDesc::rgba8(16, 16))
                .unwrap();
            let swizzled = graph
                .image("swizzled-color", ImageDesc::rgba8(16, 16))
                .unwrap();
            let final_image = graph
                .image("final-composite", ImageDesc::rgba8(16, 16))
                .unwrap();
            let unused = graph
                .image("unused-image", ImageDesc::rgba8(16, 16))
                .unwrap();
            graph
                .raster(
                    "scene-mesh-depth",
                    &crate::program::ManagedRasterProgram::mesh(),
                    &[RasterDraw {
                        vertices,
                        indices,
                        index_count: 12,
                        parameters: RasterParameters::default(),
                    }],
                    RasterAttachments {
                        colors: &[Attachment {
                            view: graph.image_view(color, 0, 0).unwrap(),
                            load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
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
            graph
                .compute("unused-branch", color, unused, ComputeTransform::Invert)
                .unwrap();
            graph
                .compute("invert-scene", color, inverted, ComputeTransform::Invert)
                .unwrap();
            graph
                .compute(
                    "swizzle-scene",
                    inverted,
                    swizzled,
                    ComputeTransform::SwapRedBlue,
                )
                .unwrap();
            graph
                .composite("sample-final", swizzled, final_image)
                .unwrap();
            graph.export_image(final_image).unwrap();
            let final_read = graph.readback_image("read-final", final_image).unwrap();
            let compiled = graph.compile().unwrap();
            #[cfg(feature = "graph-diagnostics")]
            assert!(compiled.culled_pass_names().eq(["unused-branch"]));
            assert!(
                !compiled
                    .ordered_pass_names()
                    .any(|name| name == "unused-branch")
            );
            #[cfg(feature = "graph-diagnostics")]
            let (mut producer, lowering) = runtime.submit_with_diagnostics(&compiled).unwrap();
            #[cfg(not(feature = "graph-diagnostics"))]
            let mut producer = runtime.submit(&compiled).unwrap();
            #[cfg(feature = "graph-diagnostics")]
            for field in [
                "physical_key owner=",
                "initial_scheduled revision=",
                "purpose=Vertex",
                "purpose=Index",
                "purpose=StorageRead",
                "purpose=Sampled",
                "dst_stage=VERTEX_ATTRIBUTE_INPUT",
                "dst_stage=INDEX_INPUT",
                "new_layout=COLOR_ATTACHMENT_OPTIMAL",
                "new_layout=GENERAL",
                "new_layout=SHADER_READ_ONLY_OPTIMAL",
                "dst_stage=HOST dst_access=HOST_READ",
                "accepted_on_submit revision=",
            ] {
                assert!(lowering.contains(field), "missing {field}: {lowering}");
            }
            #[cfg(feature = "graph-diagnostics")]
            assert!(!lowering.contains("unused-branch"));
            let exported = producer.export_image(final_image).unwrap();
            // No CPU wait occurs between A and B. B inherits accepted native
            // layout/access/content and uses another descriptor arena.
            let mut consumer = Graph::test_new();
            let source = consumer.import_image("pending-final", &exported).unwrap();
            let destination = consumer
                .image("consumer-inverted", ImageDesc::rgba8(16, 16))
                .unwrap();
            consumer
                .compute(
                    "consumer-invert",
                    source,
                    destination,
                    ComputeTransform::Invert,
                )
                .unwrap();
            let consumer_read = consumer
                .readback_image("consumer-read", destination)
                .unwrap();
            let consumer = consumer.compile().unwrap();
            #[cfg(feature = "graph-diagnostics")]
            let (mut pending_consumer, lowering) =
                runtime.submit_with_diagnostics(&consumer).unwrap();
            #[cfg(not(feature = "graph-diagnostics"))]
            let mut pending_consumer = runtime.submit(&consumer).unwrap();
            assert_eq!(
                runtime.execution.stats().leased,
                2,
                "one execution per graph, with two pending graphs"
            );
            #[cfg(feature = "graph-diagnostics")]
            assert!(lowering.contains(&format!(
                "imported=true import_revision={}",
                exported.revision
            )));
            #[cfg(feature = "graph-diagnostics")]
            assert!(lowering.contains("pending-final"));
            runtime.delete(exported.key()).unwrap(); // pending users retain allocation/view
            let mut consumer_result = pending_consumer
                .wait(WAIT)
                .unwrap()
                .expect("consumer must complete");
            assert_eq!(
                consumer_result.test_readback(consumer_read).unwrap(),
                expected_consumer
            );
            let mut producer_result = producer
                .wait(WAIT)
                .unwrap()
                .expect("producer must complete");
            assert_eq!(producer_result.test_readback(final_read).unwrap(), expected);
        }
        let stats = runtime.stats();
        assert_eq!(
            stats.resources_created, 8,
            "culled image must not materialize, all live storage must be reusable"
        );
        assert!(stats.resources_reused >= 8);
        assert_eq!(
            stats.execution_slots_created,
            runtime.execution.stats().capacity,
            "graph reuse does not create additional native execution slots"
        );
        assert!(stats.execution_slots_reused >= 2);
        assert_eq!(
            stats.descriptor_arenas_created,
            runtime.config.execution_slots
        );
        assert!(stats.descriptor_arenas_reused >= 2);
        assert_eq!(
            (stats.upload_live_ranges, stats.readback_live_ranges),
            (0, 0)
        );
    });
}

//! Real-GPU shader, depth and complete M2 integration contracts.

use super::*;
use crate::graph::{AttachmentOps, BufferDesc, ComputeTransform, Graph, ImageDesc};

const WAIT: Duration = Duration::from_secs(10);

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
        let diagnostics = Arc::clone(&runtime.diagnostics);
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

#[test]
#[ignore = "explicit real-GPU descriptor failure cleanup; requires Core/SyncVal"]
fn vulkan_graph_descriptor_exhaustion_releases_unsubmitted_leases() {
    real_gpu_config(
        RuntimeConfig {
            max_shader_bindings: 1,
            ..RuntimeConfig::default()
        },
        |runtime| {
            let mut graph = Graph::new();
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
                .upload_image("capacity-upload", source, bytes.clone())
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
            let mut graph = Graph::new();
            let source = graph
                .image("recovered-source", ImageDesc::rgba8(8, 8))
                .unwrap();
            let destination = graph
                .image("recovered-destination", ImageDesc::rgba8(8, 8))
                .unwrap();
            graph
                .upload_image("recovered-upload", source, bytes.clone())
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
                result.readback(read).unwrap(),
                transform(&bytes, ComputeTransform::Invert)
            );
            let recovered = runtime.stats();
            assert_eq!(recovered.descriptor_arenas_created, 1);
            assert!(recovered.descriptor_arenas_reused >= 1);
            assert_eq!(
                (recovered.upload_live_ranges, recovered.readback_live_ranges),
                (0, 0)
            );
            // Byte extent validation alone cannot establish safe indexed access.
            // A partial upload that changes one actual index must be rejected while
            // still unsubmitted, with all acquired leases canceled.
            let mut invalid = Graph::new();
            let (vertices, indices) = add_mesh(&mut invalid, false);
            invalid
                .upload_buffer(
                    "invalid-first-index",
                    indices,
                    0,
                    999u32.to_le_bytes().to_vec(),
                )
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
                    vertices,
                    indices,
                    12,
                    color,
                    depth,
                    AttachmentOps::CLEAR_STORE,
                    AttachmentOps::CLEAR_STORE,
                    [0.0, 0.0, 0.0, 1.0],
                    1.0,
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
        let mut graph = Graph::new();
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
        graph.upload_image("upload", source, bytes.clone()).unwrap();
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
        let mut compute_pipeline = None;
        for _ in 0..4 {
            let mut submission = runtime.submit(&compiled).unwrap();
            let handles = runtime.programs.as_ref().unwrap().handles();
            assert_ne!(handles[0], vk::Pipeline::null());
            assert_eq!(handles[1..], [vk::Pipeline::null(); 2]);
            assert_eq!(*compute_pipeline.get_or_insert(handles[0]), handles[0]);
            let mut result = submission
                .wait(WAIT)
                .unwrap()
                .expect("Compute must complete");
            assert_eq!(result.readback(first_read).unwrap(), expected_first);
            assert_eq!(result.readback(second_read).unwrap(), expected_second);
        }
        let stats = runtime.stats();
        assert_eq!(stats.resources_created, 3, "images/views must be reused");
        assert!(stats.resources_reused >= 9);
        assert_eq!(stats.execution_slots_created, 1);
        assert!(stats.execution_slots_reused >= 3);
        assert_eq!(stats.descriptor_arenas_created, 1);
        assert!(stats.descriptor_arenas_reused >= 3);
        assert_eq!(
            (stats.upload_live_ranges, stats.readback_live_ranges),
            (0, 0)
        );
    });
}

#[test]
#[ignore = "explicit real-GPU lazy program creation with pending work; requires Core/SyncVal"]
fn vulkan_graph_adds_program_kinds_without_replacing_pending_programs() {
    real_gpu(|runtime| {
        let mut producer = Graph::new();
        let source = producer
            .image("lazy-source", ImageDesc::rgba8(4, 4))
            .unwrap();
        let output = producer
            .image("lazy-computed", ImageDesc::rgba8(4, 4))
            .unwrap();
        let bytes = [13, 29, 87, 173].repeat(16);
        producer
            .upload_image("lazy-upload", source, bytes.clone())
            .unwrap();
        producer
            .compute("lazy-compute", source, output, ComputeTransform::Invert)
            .unwrap();
        producer.export_image(output).unwrap();
        let mut pending_compute = runtime.submit(&producer.compile().unwrap()).unwrap();
        let exported = pending_compute.export_image(output).unwrap();
        let first = runtime.programs.as_ref().unwrap().handles();
        assert_ne!(first[0], vk::Pipeline::null());
        assert_eq!(first[1..], [vk::Pipeline::null(); 2]);

        // Add composite while the Compute submission still owns its program.
        let mut consumer = Graph::new();
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
        assert_eq!(second[0], first[0]);
        assert_eq!(second[1], vk::Pipeline::null());
        assert_ne!(second[2], vk::Pipeline::null());

        // Add mesh without replacing either earlier program. All three kinds
        // coexist while the prior submissions still retain their leases.
        let mut mesh = Graph::new();
        let (vertices, indices) = add_mesh(&mut mesh, false);
        let color = mesh.image("lazy-color", ImageDesc::rgba8(16, 16)).unwrap();
        let depth = mesh
            .image("lazy-depth", ImageDesc::depth32(16, 16))
            .unwrap();
        mesh.raster(
            "lazy-mesh",
            vertices,
            indices,
            12,
            color,
            depth,
            AttachmentOps::CLEAR_STORE,
            AttachmentOps::CLEAR_STORE,
            [0.0; 4],
            1.0,
        )
        .unwrap();
        let mesh_read = mesh.readback_image("lazy-read-mesh", color).unwrap();
        let mut pending_mesh = runtime.submit(&mesh.compile().unwrap()).unwrap();
        let third = runtime.programs.as_ref().unwrap().handles();
        assert_eq!(third[0], first[0]);
        assert_eq!(third[2], second[2]);
        assert_ne!(third[1], vk::Pipeline::null());
        let mut mesh_result = pending_mesh.wait(WAIT).unwrap().expect("mesh must finish");
        assert_eq!(
            mesh_result.readback(mesh_read).unwrap(),
            expected_mesh([0; 4])
        );
        let mut composite_result = pending_composite
            .wait(WAIT)
            .unwrap()
            .expect("composite must finish");
        assert_eq!(
            composite_result.readback(read).unwrap(),
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
        .upload_buffer("upload-vertices", v, 0, vertices)
        .unwrap();
    graph
        .upload_buffer("upload-indices", i, 0, indices)
        .unwrap();
    (v, i)
}

#[test]
#[ignore = "explicit real-GPU indexed mesh/depth and attachment LOAD; requires Core/SyncVal"]
fn vulkan_graph_depth_occlusion_and_attachment_load() {
    real_gpu(|runtime| {
        for reverse in [false, true] {
            let mut graph = Graph::new();
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
                    vertices,
                    indices,
                    12,
                    color,
                    depth,
                    AttachmentOps::CLEAR_STORE,
                    AttachmentOps::CLEAR_STORE,
                    [0.0, 0.0, 0.0, 1.0],
                    1.0,
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
                .upload_buffer("upload-background-vertices", blue_vertices, 0, blue)
                .unwrap();
            graph
                .upload_buffer(
                    "upload-background-indices",
                    blue_indices,
                    0,
                    index_bytes(&[0, 1, 2, 2, 3, 0]),
                )
                .unwrap();
            graph
                .raster(
                    "mesh-load",
                    blue_vertices,
                    blue_indices,
                    6,
                    color,
                    depth,
                    AttachmentOps::LOAD_STORE,
                    AttachmentOps::LOAD_STORE,
                    [1.0; 4],
                    0.0,
                )
                .unwrap();
            let loaded = graph.readback_image("read-after-load", color).unwrap();
            let compiled = graph.compile().unwrap();
            let mut submission = runtime.submit(&compiled).unwrap();
            let mut result = submission.wait(WAIT).unwrap().expect("mesh must complete");
            let (expected_vertices, expected_indices) = mesh(reverse);
            assert_eq!(result.readback(vertex_proof).unwrap(), expected_vertices);
            assert_eq!(result.readback(index_proof).unwrap(), expected_indices);
            assert_eq!(
                result.readback(initial).unwrap(),
                expected_mesh([0, 0, 0, 255]),
                "depth must be independent of triangle order"
            );
            assert_eq!(
                result.readback(loaded).unwrap(),
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
        let expected_scene = expected_mesh([0, 0, 0, 255]);
        let expected = transform(
            &transform(&expected_scene, ComputeTransform::Invert),
            ComputeTransform::SwapRedBlue,
        );
        let expected_consumer = transform(&expected, ComputeTransform::Invert);
        for reverse in [false, true] {
            let mut graph = Graph::new();
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
                    vertices,
                    indices,
                    12,
                    color,
                    depth,
                    AttachmentOps::CLEAR_STORE,
                    AttachmentOps::CLEAR_STORE,
                    [0.0, 0.0, 0.0, 1.0],
                    1.0,
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
            assert_eq!(compiled.culled_pass_names(), ["unused-branch"]);
            assert!(!compiled.ordered_pass_names().contains(&"unused-branch"));
            let mut producer = runtime.submit(&compiled).unwrap();
            let exported = producer.export_image(final_image).unwrap();
            // No CPU wait occurs between A and B. B inherits accepted native
            // layout/access/content and uses another descriptor arena.
            let mut consumer = Graph::new();
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
            let mut pending_consumer = runtime.submit(&consumer.compile().unwrap()).unwrap();
            runtime.delete(exported.key()).unwrap(); // pending users retain allocation/view
            let mut consumer_result = pending_consumer
                .wait(WAIT)
                .unwrap()
                .expect("consumer must complete");
            assert_eq!(
                consumer_result.readback(consumer_read).unwrap(),
                expected_consumer
            );
            let mut producer_result = producer
                .wait(WAIT)
                .unwrap()
                .expect("producer must complete");
            assert_eq!(producer_result.readback(final_read).unwrap(), expected);
        }
        let stats = runtime.stats();
        assert_eq!(
            stats.resources_created, 8,
            "culled image must not materialize, all live storage must be reusable"
        );
        assert!(stats.resources_reused >= 8);
        assert_eq!(
            stats.execution_slots_created, 2,
            "one execution per graph, with two pending graphs"
        );
        assert!(stats.execution_slots_reused >= 2);
        assert_eq!(stats.descriptor_arenas_created, 2);
        assert!(stats.descriptor_arenas_reused >= 2);
        assert_eq!(
            (stats.upload_live_ranges, stats.readback_live_ranges),
            (0, 0)
        );
    });
}

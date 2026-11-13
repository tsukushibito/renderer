//! Real-GPU repeated frame ownership, reuse and history scenarios.

use super::*;
use std::collections::HashMap;
use std::time::Instant;

use crate::graph::{BufferDesc, ComputeTransform, Graph, ImageDesc};

const WAIT: Duration = Duration::from_secs(10);

fn real_gpu(config: RuntimeConfig, test: impl FnOnce(&mut VulkanRuntime)) {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(config).expect("real Vulkan runtime is required");
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );
        let diagnostics = runtime.diagnostics.clone();
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        test(&mut runtime);
        assert!(
            runtime.drain_frames(WAIT).unwrap(),
            "explicit frame shutdown timed out"
        );
        drop(runtime);
        let report = diagnostics.snapshot();
        assert!(
            report.is_clean(),
            "validation through shutdown: {report:#?}"
        );
    });
}

fn completion(runtime: &mut VulkanRuntime) -> FrameCompletion {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(completed) = runtime.poll_frame().unwrap() {
            return completed;
        }
        assert!(
            Instant::now() < deadline,
            "frame did not complete within test budget"
        );
        std::thread::yield_now();
    }
}

fn transformed(bytes: &[u8], operation: ComputeTransform) -> Vec<u8> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| match operation {
            ComputeTransform::Invert => [255 - pixel[0], 255 - pixel[1], 255 - pixel[2], pixel[3]],
            ComputeTransform::SwapRedBlue => [pixel[2], pixel[1], pixel[0], pixel[3]],
        })
        .collect()
}

#[test]
#[ignore = "explicit repeated real-GPU frame reuse and unread readback lifetime; requires Core/SyncVal"]
fn vulkan_frames_reuse_resources_while_cpu_keeps_old_readbacks() {
    real_gpu(
        RuntimeConfig {
            execution_slots: 3,
            resource_capacity: 9,
            ..RuntimeConfig::default()
        },
        |runtime| {
            let mut retained = Vec::new();
            let mut warm = None;
            for round in 0..4 {
                let mut expected = HashMap::new();
                for lane in 0..3 {
                    let value = (round * 3 + lane + 1) as u8;
                    let buffer_bytes = vec![value; 64];
                    let image_bytes = [value, value + 3, value + 7, 255].repeat(64);
                    let mut graph = Graph::new();
                    let buffer = graph
                        .buffer("frame bytes", BufferDesc { size: 64 })
                        .unwrap();
                    let source = graph.image("frame source", ImageDesc::rgba8(8, 8)).unwrap();
                    let target = graph
                        .image("frame compute", ImageDesc::rgba8(8, 8))
                        .unwrap();
                    graph
                        .upload_buffer("frame byte upload", buffer, 0, buffer_bytes.clone())
                        .unwrap();
                    graph
                        .upload_image("frame image upload", source, image_bytes.clone())
                        .unwrap();
                    graph
                        .compute("frame invert", source, target, ComputeTransform::Invert)
                        .unwrap();
                    let buffer_read = graph
                        .readback_buffer("frame byte result", buffer, 0, 64)
                        .unwrap();
                    let image_read = graph.readback_image("frame image result", target).unwrap();
                    let submitted = runtime.submit_frame(&graph.compile().unwrap()).unwrap();
                    expected.insert(
                        submitted.id(),
                        (
                            buffer_read,
                            buffer_bytes,
                            image_read,
                            transformed(&image_bytes, ComputeTransform::Invert),
                        ),
                    );
                    drop(submitted);
                }
                // These owners remain in the runtime even if the GPU has already
                // finished. No wall-clock assumption establishes a pending state.
                assert_eq!(runtime.pending_frames(), 3);
                for _ in 0..3 {
                    let completed = completion(runtime);
                    let expectation = expected
                        .remove(&completed.id())
                        .expect("matching accepted frame");
                    retained.push((completed.result, expectation));
                }
                assert!(expected.is_empty());
                assert_eq!(runtime.pending_frames(), 0);
                let stats = runtime.stats();
                assert_eq!(stats.upload_live_ranges, 0);
                assert_eq!(stats.readback_live_ranges, (round + 1) * 6);
                if let Some((resources, execution, descriptors)) = warm {
                    assert_eq!(stats.resources_created, resources);
                    assert_eq!(stats.execution_slots_created, execution);
                    assert_eq!(stats.descriptor_arenas_created, descriptors);
                } else {
                    assert_eq!(stats.resources_created, 9);
                    assert_eq!(stats.execution_slots_created, 3);
                    assert_eq!(stats.descriptor_arenas_created, 3);
                    warm = Some((
                        stats.resources_created,
                        stats.execution_slots_created,
                        stats.descriptor_arenas_created,
                    ));
                }
            }
            let stats = runtime.stats();
            assert!(stats.resources_reused >= 27);
            assert!(stats.execution_slots_reused >= 9);
            assert!(stats.descriptor_arenas_reused >= 9);
            // Native execution, upload, images/Views and descriptor arenas have all
            // been reused before these old CPU results are read for the first time.
            for (mut result, (buffer_read, bytes, image_read, pixels)) in retained {
                assert_eq!(result.readback(buffer_read).unwrap(), bytes);
                assert_eq!(result.readback(image_read).unwrap(), pixels);
            }
            assert_eq!(runtime.stats().readback_live_ranges, 0);
        },
    );
}

#[test]
#[ignore = "explicit real-GPU pending deletion, size replacement and staging recovery; requires Core/SyncVal"]
fn vulkan_frames_delete_pending_resources_and_recover_readback_capacity() {
    real_gpu(
        RuntimeConfig {
            execution_slots: 2,
            resource_capacity: 2,
            staging_bytes: 4096,
            ..RuntimeConfig::default()
        },
        |runtime| {
            let make = |size: u64, value: u8| {
                let mut graph = Graph::new();
                let buffer = graph
                    .buffer("changing buffer", BufferDesc { size })
                    .unwrap();
                graph
                    .upload_buffer("changing upload", buffer, 0, vec![value; size as usize])
                    .unwrap();
                graph.export_buffer(buffer).unwrap();
                let read = graph
                    .readback_buffer("changing readback", buffer, 0, size)
                    .unwrap();
                (graph.compile().unwrap(), buffer, read)
            };
            let (first, first_buffer, first_read) = make(2048, 17);
            let submitted = runtime.submit_frame(&first).unwrap();
            let first_id = submitted.id();
            let first_import = submitted.export_buffer(first_buffer).unwrap();
            let first_key = first_import.key();
            runtime.delete(first_key).unwrap();
            assert_eq!(
                runtime.refresh_buffer(first_key).unwrap_err().kind(),
                RuntimeErrorKind::StaleResource
            );
            assert!(Arc::strong_count(&runtime.entries[first_key.index as usize].physical) > 1);
            drop(submitted);
            let (second, second_buffer, second_read) = make(2048, 41);
            let submitted = runtime.submit_frame(&second).unwrap();
            let second_key = submitted.export_buffer(second_buffer).unwrap().key();
            assert_ne!(
                first_key.index, second_key.index,
                "unobserved submitted owner cannot be reused"
            );
            runtime.delete(second_key).unwrap();
            drop(submitted);
            let mut first_result = None;
            let mut second_result = None;
            for _ in 0..2 {
                let completed = completion(runtime);
                if completed.id() == first_id {
                    first_result = Some(completed.result);
                } else {
                    second_result = Some(completed.result);
                }
            }
            assert_eq!(runtime.stats().upload_live_ranges, 0);
            assert_eq!(runtime.stats().readback_live_ranges, 2);
            let (replacement, replacement_buffer, replacement_read) = make(1024, 93);
            let failed = match runtime.submit_frame(&replacement) {
                Ok(_) => panic!("unconsumed results must retain the entire readback capacity"),
                Err(error) => error,
            };
            assert_eq!(failed.kind(), RuntimeErrorKind::CapacityExhausted);
            assert_eq!(runtime.pending_frames(), 0);
            assert_eq!(
                runtime.stats().upload_live_ranges,
                0,
                "rejected preparation releases upload lease"
            );
            let mut retained = first_result.expect("first completed result");
            assert_eq!(retained.readback(first_read).unwrap(), vec![17; 2048]);
            assert_eq!(runtime.stats().readback_live_ranges, 1);
            let submitted = runtime.submit_frame(&replacement).unwrap();
            let replacement_key = submitted.export_buffer(replacement_buffer).unwrap().key();
            assert!(
                replacement_key.index == first_key.index
                    || replacement_key.index == second_key.index
            );
            let previous = if replacement_key.index == first_key.index {
                first_key
            } else {
                second_key
            };
            assert!(replacement_key.generation > previous.generation);
            assert_eq!(
                runtime.entries.len(),
                2,
                "size change replaces an idle bounded entry"
            );
            runtime.delete(replacement_key).unwrap();
            drop(submitted);
            let mut result = completion(runtime).result;
            assert_eq!(result.readback(replacement_read).unwrap(), vec![93; 1024]);
            assert_eq!(
                second_result.unwrap().readback(second_read).unwrap(),
                vec![41; 2048]
            );
            assert_eq!(runtime.stats().readback_live_ranges, 0);
            assert_eq!(runtime.stats().registered_resources, 0);
        },
    );
}

#[test]
#[ignore = "explicit real-GPU history import/export, reset and resize; requires Core/SyncVal"]
fn vulkan_frames_history_chains_pending_frames_and_invalidates_old_extents() {
    real_gpu(
        RuntimeConfig {
            execution_slots: 3,
            resource_capacity: 12,
            ..RuntimeConfig::default()
        },
        |runtime| {
            let mut previous: Option<ImportImage> = None;
            for (phase, desc) in [
                ImageDesc::rgba8(8, 8),
                ImageDesc::rgba8(8, 8),
                ImageDesc::rgba8(5, 3),
            ]
            .into_iter()
            .enumerate()
            {
                // The previous phase invalidates its history while its last
                // reader is still runtime-owned. This phase starts with fresh
                // content, once at the same extent and once after a resize.
                assert!(previous.is_none());
                let mut pixels =
                    [13 + phase as u8, 47, 91, 213].repeat((desc.width * desc.height) as usize);
                for batch in 0..2 {
                    let mut expected = HashMap::new();
                    for lane in 0..3 {
                        let mut graph = Graph::new();
                        let source = if let Some(old) = &previous {
                            let fresh = runtime.refresh_image(old.key()).unwrap();
                            graph.import_image("previous history", &fresh).unwrap()
                        } else {
                            let source = graph.image("first history input", desc).unwrap();
                            graph
                                .upload_image("initialize history", source, pixels.clone())
                                .unwrap();
                            source
                        };
                        let target = graph.image("next history", desc).unwrap();
                        let operation = if (batch * 3 + lane) % 2 == 0 {
                            ComputeTransform::Invert
                        } else {
                            ComputeTransform::SwapRedBlue
                        };
                        graph
                            .compute("history transform", source, target, operation)
                            .unwrap();
                        graph.export_image(target).unwrap();
                        let read = graph.readback_image("history result", target).unwrap();
                        let submitted = runtime.submit_frame(&graph.compile().unwrap()).unwrap();
                        let current = submitted.export_image(target).unwrap();
                        if let Some(old) = previous.replace(current) {
                            runtime.delete(old.key()).unwrap();
                            assert_eq!(
                                runtime.refresh_image(old.key()).unwrap_err().kind(),
                                RuntimeErrorKind::StaleResource
                            );
                        }
                        pixels = transformed(&pixels, operation);
                        expected.insert(submitted.id(), (read, pixels.clone()));
                        drop(submitted);
                    }
                    // All three consumers have been accepted without host waiting
                    // between their import/export handoffs.
                    assert_eq!(runtime.pending_frames(), 3);
                    for _ in 0..3 {
                        let mut completed = completion(runtime);
                        let (read, bytes) = expected.remove(&completed.id()).unwrap();
                        assert_eq!(completed.result.readback(read).unwrap(), bytes);
                    }
                }
                // Read-only work advances native queue state, not the content
                // revision. Independently accepted uses share the compiled plan.
                let current = runtime
                    .refresh_image(previous.as_ref().unwrap().key())
                    .unwrap();
                let mut graph = Graph::new();
                let history = graph.import_image("readonly history", &current).unwrap();
                let read = graph
                    .readback_image("readonly history result", history)
                    .unwrap();
                let shared_plan = graph.compile().unwrap();
                drop(runtime.submit_frame(&shared_plan).unwrap());
                drop(runtime.submit_frame(&shared_plan).unwrap());
                assert_eq!(runtime.refresh_image(current.key()).unwrap(), current);
                let old = previous.take().unwrap();
                runtime.delete(old.key()).unwrap();
                assert_eq!(runtime.pending_frames(), 2);
                assert_eq!(
                    runtime.refresh_image(old.key()).unwrap_err().kind(),
                    RuntimeErrorKind::StaleResource
                );
                // Invalidating a history registration must not retire the
                // accepted reader or overwrite its still-unconsumed output.
                for _ in 0..2 {
                    assert_eq!(completion(runtime).result.readback(read).unwrap(), pixels);
                }
            }
            assert_eq!(runtime.stats().registered_resources, 0);
            assert_eq!(runtime.stats().readback_live_ranges, 0);
        },
    );
}

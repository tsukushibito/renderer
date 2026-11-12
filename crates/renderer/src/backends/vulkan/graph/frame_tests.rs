//! Explicit GPU coverage of runtime-owned frame submission and result lifetime.

use super::*;
use crate::graph::{BufferDesc, Graph};
use std::collections::HashMap;
use std::time::Instant;

#[test]
#[ignore = "explicit real-GPU frame ownership, bounded retirement and readback test"]
fn vulkan_frames_metadata_drop_capacity_poll_and_explicit_drain() {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(RuntimeConfig {
            execution_slots: 2,
            resource_capacity: 4,
            staging_bytes: 4096,
            max_shader_bindings: 2,
        })
        .expect("real Vulkan runtime is required");
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU,
            "CPU fallback is not real-GPU acceptance"
        );
        let diagnostics = Arc::clone(&runtime.diagnostics);
        crate::backends::vulkan::assert_real_validation(&diagnostics);

        let bytes = [17, 29, 83, 211].repeat(4);
        let mut graph = Graph::new();
        let buffer = graph
            .buffer("frame-buffer", BufferDesc { size: 16 })
            .unwrap();
        graph
            .upload_buffer("frame-upload", buffer, 0, bytes.clone())
            .unwrap();
        let read = graph.readback_buffer("frame-read", buffer, 0, 16).unwrap();
        graph.export_buffer(buffer).unwrap();
        let plan = graph.compile().unwrap();

        let first = runtime.submit_frame(&plan).unwrap();
        let first_id = first.id();
        let first_key = first.export_buffer(buffer).unwrap().key();
        let mut foreign = Graph::new();
        let foreign_buffer = foreign.buffer("foreign", BufferDesc { size: 16 }).unwrap();
        assert_eq!(
            first.export_buffer(foreign_buffer).unwrap_err().kind(),
            RuntimeErrorKind::WrongOwner
        );
        drop(first);
        assert_eq!(
            runtime.pending_frames(),
            1,
            "metadata drop neither waits nor removes runtime ownership"
        );
        runtime.delete(first_key).unwrap();

        let second = runtime.submit_frame(&plan).unwrap();
        let second_id = second.id();
        assert_ne!(first_id, second_id);
        runtime
            .delete(second.export_buffer(buffer).unwrap().key())
            .unwrap();
        drop(second);
        assert_eq!(runtime.pending_frames(), 2);
        let created = runtime.stats().resources_created;
        assert_eq!(
            runtime.submit_frame(&plan).unwrap_err().kind(),
            RuntimeErrorKind::CapacityExhausted,
            "even a signaled but unobserved frame still occupies bookkeeping"
        );
        assert_eq!(runtime.pending_frames(), 2);
        assert_eq!(runtime.stats().resources_created, created);

        let mut complete = HashMap::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while runtime.pending_frames() != 0 {
            if let Some(frame) = runtime.poll_frame().unwrap() {
                assert!(complete.insert(frame.id(), frame.result).is_none());
            } else {
                assert!(Instant::now() < deadline, "GPU frames must finish");
                std::thread::yield_now();
            }
        }
        assert_eq!(complete.len(), 2);
        let retired = runtime.stats();
        assert_eq!(retired.upload_live_ranges, 0);
        assert_eq!(retired.readback_live_ranges, 2);

        // Native execution and graph resources can be reused while completed
        // CPU results still retain their independent readback ranges.
        let third = runtime.submit_frame(&plan).unwrap();
        runtime
            .delete(third.export_buffer(buffer).unwrap().key())
            .unwrap();
        drop(third);
        if !runtime.drain_frames(Duration::ZERO).unwrap() {
            assert_eq!(runtime.pending_frames(), 1);
            assert!(runtime.drain_frames(Duration::from_secs(10)).unwrap());
        }
        assert_eq!(runtime.pending_frames(), 0);
        assert_eq!(runtime.stats().readback_live_ranges, 2);
        assert_eq!(runtime.stats().execution_slots_created, 2);
        assert!(runtime.stats().execution_slots_reused >= 1);
        for id in [first_id, second_id] {
            assert_eq!(
                complete.get_mut(&id).unwrap().readback(read).unwrap(),
                bytes
            );
        }
        assert_eq!(runtime.stats().readback_live_ranges, 0);
        assert!(runtime.poll_frame().unwrap().is_none());
        assert!(runtime.drain_frames(Duration::ZERO).unwrap());
        drop((complete, plan, graph, runtime));
        let report = diagnostics.snapshot();
        assert!(
            report.is_clean(),
            "Core/SyncVal and callback failures, including shutdown: {report:#?}"
        );
    });
}

#[test]
#[ignore = "explicit real-GPU budgeted frame wait and retained readback results; requires Core/SyncVal"]
fn vulkan_frames_wait_returns_one_result_without_discarding_other_frames() {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(RuntimeConfig {
            execution_slots: 2,
            resource_capacity: 2,
            staging_bytes: 4096,
            max_shader_bindings: 2,
        })
        .expect("real Vulkan runtime is required");
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU,
            "CPU fallback is not real-GPU acceptance"
        );
        let diagnostics = Arc::clone(&runtime.diagnostics);
        crate::backends::vulkan::assert_real_validation(&diagnostics);

        let bytes = [7, 43, 101, 239].repeat(16);
        let mut graph = Graph::new();
        let buffer = graph
            .buffer("wait frame buffer", BufferDesc { size: 64 })
            .unwrap();
        graph
            .upload_buffer("wait frame upload", buffer, 0, bytes.clone())
            .unwrap();
        let read = graph
            .readback_buffer("wait frame readback", buffer, 0, 64)
            .unwrap();
        let plan = graph.compile().unwrap();
        let first = runtime.submit_frame(&plan).unwrap().id();
        let second = runtime.submit_frame(&plan).unwrap().id();
        assert_eq!(runtime.pending_frames(), 2);

        // A zero timeout is a poll: an already finished frame may be returned,
        // but success or timeout must not consume any other result.
        let mut results = HashMap::new();
        if let Some(completed) = runtime.wait_frame(Duration::ZERO).unwrap() {
            results.insert(completed.id(), completed.result);
        }
        assert_eq!(runtime.pending_frames(), 2 - results.len());
        while runtime.pending_frames() != 0 {
            let pending = runtime.pending_frames();
            let completed = runtime
                .wait_frame(Duration::from_secs(10))
                .unwrap()
                .expect("one accepted frame must finish within the wait budget");
            assert!(results.insert(completed.id(), completed.result).is_none());
            assert_eq!(runtime.pending_frames(), pending - 1);
        }
        assert_eq!(runtime.stats().readback_live_ranges, 2);
        assert_eq!(runtime.stats().upload_live_ranges, 0);
        for id in [first, second] {
            assert_eq!(results.get_mut(&id).unwrap().readback(read).unwrap(), bytes);
        }
        assert_eq!(runtime.stats().readback_live_ranges, 0);
        assert!(
            runtime
                .wait_frame(Duration::from_secs(1))
                .unwrap()
                .is_none()
        );
        drop((results, graph, plan, runtime));
        let report = diagnostics.snapshot();
        assert!(
            report.is_clean(),
            "Core/SyncVal and callback failures, including shutdown: {report:#?}"
        );
    });
}

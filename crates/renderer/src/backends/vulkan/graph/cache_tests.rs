//! Cache policy and real-GPU descriptor churn, pressure and failure regressions.

use super::*;
use crate::graph::{Graph, ImageDesc};

const WAIT: Duration = Duration::from_secs(10);

fn real_gpu(capacity: usize, slots: usize, test: impl FnOnce(&mut VulkanRuntime)) {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(RuntimeConfig {
            execution_slots: slots,
            resource_capacity: capacity,
            staging_bytes: 4096,
            max_shader_bindings: 1,
        })
        .expect("real Vulkan runtime is required");
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );
        let diagnostics = Arc::clone(&runtime.diagnostics);
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        test(&mut runtime);
        drop(runtime);
        let report = diagnostics.snapshot();
        assert!(
            report.is_clean(),
            "validation through shutdown: {report:#?}"
        );
    });
}

fn image_plan(size: u32, value: u8) -> (CompiledGraph, ImageHandle, PassHandle) {
    let mut graph = Graph::new();
    let image = graph
        .image("cached image", ImageDesc::rgba8(size, size))
        .unwrap();
    graph
        .upload_image("upload", image, vec![value; (size * size * 4) as usize])
        .unwrap();
    let read = graph.readback_image("read", image).unwrap();
    graph.export_image(image).unwrap();
    (graph.compile().unwrap(), image, read)
}

fn check_result(submission: &mut Submission, read: PassHandle, size: u32, value: u8) {
    let mut result = submission
        .wait(WAIT)
        .unwrap()
        .expect("GPU image operation must complete");
    assert_eq!(
        result.readback(read).unwrap(),
        vec![value; (size * size * 4) as usize]
    );
}

fn complete_image(runtime: &mut VulkanRuntime, size: u32, value: u8) -> ResourceKey {
    let (plan, image, read) = image_plan(size, value);
    let mut submission = runtime.submit(&plan).unwrap();
    let key = submission.export_image(image).unwrap().key();
    runtime.delete(key).unwrap();
    check_result(&mut submission, read, size, value);
    key
}

#[test]
#[ignore = "explicit real-GPU alternating cache descriptors; requires Core/SyncVal"]
fn vulkan_graph_cache_retains_alternating_image_extents() {
    real_gpu(8, 1, |runtime| {
        for frame in 0..8 {
            let size = if frame % 2 == 0 { 2 } else { 3 };
            complete_image(runtime, size, frame + 1);
        }
        assert_eq!(runtime.stats().resources_created, 2);
        assert_eq!(runtime.stats().resources_reused, 6);
        assert_eq!(runtime.entries.len(), 2);
        assert_eq!(runtime.memory_stats().unwrap().cached_logical_bytes, 52);
    });
}

#[test]
#[ignore = "explicit real-GPU cache eviction and generations; requires Core/SyncVal"]
fn vulkan_graph_cache_pressure_preserves_recent_descriptors_and_generations() {
    real_gpu(2, 1, |runtime| {
        let first_a = complete_image(runtime, 2, 11);
        let first_b = complete_image(runtime, 3, 22);
        let second_a = complete_image(runtime, 2, 33);
        assert_eq!(second_a.index, first_a.index);
        assert_eq!(second_a.generation, first_a.generation + 1);
        let c = complete_image(runtime, 4, 44);
        assert_eq!(c.index, first_b.index, "replace B, not recently reused A");
        assert_eq!(c.generation, first_b.generation + 1);
        let third_a = complete_image(runtime, 2, 55);
        assert_eq!(third_a.index, first_a.index);
        assert_eq!(third_a.generation, second_a.generation + 1);
        assert_eq!(runtime.stats().resources_created, 3);
        assert_eq!(runtime.stats().resources_reused, 2);
        assert_eq!(runtime.entries.len(), 2);

        // A saturated generation cannot be reused even for the same descriptor.
        runtime.entries[c.index as usize].generation = u32::MAX;
        let replacement = complete_image(runtime, 4, 66);
        assert_eq!(replacement.index, third_a.index);
        assert_eq!(replacement.generation, third_a.generation + 1);
        runtime.entries[replacement.index as usize].generation = u32::MAX;
        let (plan, _, _) = image_plan(4, 77);
        assert_eq!(
            runtime.submit(&plan).err().unwrap().kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(runtime.stats().resources_created, 4);
    });
}

#[test]
#[ignore = "explicit real-GPU registered and pending cache protection; requires Core/SyncVal"]
fn vulkan_graph_cache_never_evicts_registered_or_unobserved_pending_resources() {
    real_gpu(2, 3, |runtime| {
        let (first_plan, first_image, first_read) = image_plan(2, 17);
        let mut first = runtime.submit(&first_plan).unwrap();
        let first_key = first.export_image(first_image).unwrap().key();
        let (second_plan, second_image, second_read) = image_plan(3, 29);
        let mut second = runtime.submit(&second_plan).unwrap();
        let second_key = second.export_image(second_image).unwrap().key();
        runtime.delete(second_key).unwrap();
        let (third_plan, _, _) = image_plan(4, 41);
        assert_eq!(
            runtime.submit(&third_plan).err().unwrap().kind(),
            RuntimeErrorKind::CapacityExhausted,
            "a signaled Fence does not remove the unobserved pending lease"
        );
        assert_eq!(runtime.stats().resources_created, 2);
        check_result(&mut second, second_read, 3, 29);
        let third_key = complete_image(runtime, 4, 41);
        assert_eq!(third_key.index, second_key.index);
        assert_eq!(third_key.generation, second_key.generation + 1);
        check_result(&mut first, first_read, 2, 17);
        let same_descriptor = complete_image(runtime, 2, 53);
        assert_ne!(same_descriptor.index, first_key.index);
        assert!(runtime.refresh_image(first_key).is_ok());
        runtime.delete(first_key).unwrap();
    });
}

#[test]
#[ignore = "explicit real-GPU failed cache replacement retains its victim; requires Core/SyncVal"]
fn vulkan_graph_cache_failed_replacement_preserves_existing_allocation() {
    real_gpu(1, 1, |runtime| {
        let original = complete_image(runtime, 2, 7);
        let entry = &runtime.entries[original.index as usize];
        let native_image = entry.physical.image().image;
        let accepted_serial = entry.physical.lock().scheduled.serial;
        // Exercise the native resource boundary directly: Graph names are user
        // input, and a failed constructor must not destroy the selected victim.
        let error = runtime
            .acquire(ResourceDesc::Image(ImageDesc::rgba8(3, 3)), "invalid\0name")
            .err()
            .expect("invalid naming input must reject replacement");
        assert_eq!(error.kind(), RuntimeErrorKind::InvalidInput);
        let entry = &runtime.entries[original.index as usize];
        assert_eq!(entry.generation, original.generation);
        assert_eq!(entry.physical.image().image, native_image);
        assert_eq!(entry.physical.lock().scheduled.serial, accepted_serial);
        assert_eq!(runtime.stats().resources_created, 1);
        assert_eq!(runtime.entries.len(), 1);
        let reused = complete_image(runtime, 2, 19);
        assert_eq!(reused.index, original.index);
        assert_eq!(reused.generation, original.generation + 1);
        assert_eq!(runtime.stats().resources_created, 1);
        assert_eq!(runtime.stats().resources_reused, 1);
    });
}

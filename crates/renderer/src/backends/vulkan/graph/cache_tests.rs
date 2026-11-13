//! Cache policy and real-GPU descriptor churn, pressure and failure regressions.

use super::*;
use crate::graph::{BufferDesc, Graph, ImageDesc};

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
        let diagnostics = runtime.diagnostics.clone();
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
        let native_image = entry.physical().image().image;
        let accepted_serial = entry.physical().lock().scheduled.serial;
        // Exercise the native resource boundary directly: Graph names are user
        // input, and a failed constructor must not destroy the selected victim.
        let error = runtime
            .acquire(ResourceDesc::Image(ImageDesc::rgba8(3, 3)), "invalid\0name")
            .err()
            .expect("invalid naming input must reject replacement");
        assert_eq!(error.kind(), RuntimeErrorKind::InvalidInput);
        let entry = &runtime.entries[original.index as usize];
        assert_eq!(entry.generation, original.generation);
        assert_eq!(entry.physical().image().image, native_image);
        assert_eq!(entry.physical().lock().scheduled.serial, accepted_serial);
        assert_eq!(runtime.stats().resources_created, 1);
        assert_eq!(runtime.entries.len(), 1);
        let reused = complete_image(runtime, 2, 19);
        assert_eq!(reused.index, original.index);
        assert_eq!(reused.generation, original.generation + 1);
        assert_eq!(runtime.stats().resources_created, 1);
        assert_eq!(runtime.stats().resources_reused, 1);
    });
}

#[test]
#[ignore = "explicit real-GPU idle trim ownership and native memory test; requires Core/SyncVal"]
fn vulkan_graph_trim_releases_idle_working_set_and_preserves_live_owners() {
    real_gpu(8, 3, |runtime| {
        let baseline = runtime.memory_stats().unwrap();
        let first = complete_image(runtime, 4, 11);
        let second = complete_image(runtime, 8, 22);
        let registered = runtime
            .create_buffer("registered", BufferDesc { size: 64 })
            .unwrap();
        let leased = runtime
            .create_buffer("escaped lease", BufferDesc { size: 96 })
            .unwrap();
        let lease = runtime.lease(leased.key()).unwrap();
        runtime.delete(leased.key()).unwrap();

        let (plan, image, read) = image_plan(3, 33);
        let mut pending = runtime.submit(&plan).unwrap();
        let pending_key = pending.export_image(image).unwrap().key();
        runtime.delete(pending_key).unwrap();
        let before = runtime.memory_stats().unwrap();
        assert_eq!(before.cached_logical_bytes, 4 * 4 * 4 + 8 * 8 * 4);
        assert_eq!(before.registered_logical_bytes, 64);
        assert_eq!(before.retained_logical_bytes, 96 + 3 * 3 * 4);
        assert_eq!(
            before.vma_allocation_count,
            baseline.vma_allocation_count + 5
        );

        assert_eq!(runtime.trim_idle_resources(), 2);
        let trimmed = runtime.memory_stats().unwrap();
        assert_eq!(trimmed.cached_logical_bytes, 0);
        assert_eq!(trimmed.registered_logical_bytes, 64);
        assert_eq!(
            trimmed.retained_logical_bytes,
            before.retained_logical_bytes
        );
        assert_eq!(
            trimmed.vma_allocation_count,
            before.vma_allocation_count - 2
        );
        assert!(trimmed.vma_allocation_bytes < before.vma_allocation_bytes);
        assert!(pending.is_pending(), "trim must not poll or wait for work");
        assert!(runtime.refresh_buffer(registered.key()).is_ok());
        assert_eq!(runtime.trim_idle_resources(), 0);
        for key in [first, second, pending_key, leased.key()] {
            assert_eq!(
                runtime.refresh_image(key).err().unwrap().kind(),
                RuntimeErrorKind::StaleResource
            );
        }

        // A rejected constructor must leave the trimmed slot and its old
        // generation untouched. Its next successful creation gets a new key.
        assert_eq!(
            runtime
                .acquire(ResourceDesc::Buffer(BufferDesc { size: 0 }), "invalid size")
                .err()
                .unwrap()
                .kind(),
            RuntimeErrorKind::InvalidInput
        );
        let replacement = runtime
            .create_buffer("after trim", BufferDesc { size: 80 })
            .unwrap();
        assert_eq!(replacement.key().index, first.index);
        assert_eq!(replacement.key().generation, first.generation + 1);
        assert_eq!(
            runtime.entries.len(),
            5,
            "fill holes before registry growth"
        );
        assert_eq!(
            runtime.refresh_image(first).unwrap_err().kind(),
            RuntimeErrorKind::StaleResource
        );

        check_result(&mut pending, read, 3, 33);
        assert_eq!(runtime.trim_idle_resources(), 1);
        assert_eq!(runtime.memory_stats().unwrap().retained_logical_bytes, 96);
        drop(lease);
        assert_eq!(runtime.trim_idle_resources(), 1);
        runtime.delete(registered.key()).unwrap();
        runtime.delete(replacement.key()).unwrap();
        assert_eq!(runtime.trim_idle_resources(), 2);
        let final_memory = runtime.memory_stats().unwrap();
        assert_eq!(
            final_memory.vma_allocation_count,
            baseline.vma_allocation_count
        );
        assert_eq!(
            final_memory.vma_allocation_bytes,
            baseline.vma_allocation_bytes
        );
        assert_eq!(final_memory.cached_logical_bytes, 0);
        assert_eq!(final_memory.retained_logical_bytes, 0);
        eprintln!(
            "idle trim memory: baseline={baseline:?}; before={before:?}; trimmed={trimmed:?}; final={final_memory:?}"
        );
    });
}

#[test]
#[ignore = "explicit real-GPU saturated generation trim test; requires Core/SyncVal"]
fn vulkan_graph_trim_releases_saturated_generations_without_reviving_keys() {
    real_gpu(1, 1, |runtime| {
        let baseline = runtime.memory_stats().unwrap();
        let key = complete_image(runtime, 2, 17);
        runtime.entries[key.index as usize].generation = u32::MAX;
        assert_eq!(runtime.trim_idle_resources(), 1);
        assert_eq!(runtime.trim_idle_resources(), 0);
        assert_eq!(
            runtime.memory_stats().unwrap().vma_allocation_count,
            baseline.vma_allocation_count
        );
        let (plan, _, _) = image_plan(2, 29);
        assert_eq!(
            runtime.submit(&plan).err().unwrap().kind(),
            RuntimeErrorKind::CapacityExhausted,
            "an exhausted slot remains a tombstone after releasing its allocation"
        );
        assert_eq!(
            runtime.refresh_image(key).unwrap_err().kind(),
            RuntimeErrorKind::StaleResource
        );
    });
}

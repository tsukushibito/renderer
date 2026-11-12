use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::backends::vulkan::buffer::BufferRole;
use crate::backends::vulkan::device::VulkanDevice;
use crate::backends::vulkan::diagnostics::Diagnostics;
use crate::backends::vulkan::image::ImageDesc;
use crate::backends::vulkan::instance::{LOG_TARGET, VulkanInstance};
use crate::backends::vulkan::memory::VulkanAllocator;

#[test]
fn copy_ranges_check_offsets_bounds_overflow_and_nonempty_work() {
    assert_eq!(copy_range(512, 0, 257).unwrap(), 0..257);
    assert_eq!(copy_range(512, 4, 257).unwrap(), 4..261);
    for (capacity, offset, size) in [
        (512, 0, 0),
        (512, 2, 4),
        (512, 512, 1),
        (512, 508, 5),
        (u64::MAX, u64::MAX - 3, 8),
    ] {
        assert!(copy_range(capacity, offset, size).is_err());
    }
}

#[test]
fn identifiers_cannot_name_another_batch_or_missing_resource() {
    assert_eq!(check_id(17, 17, 1, 2).unwrap(), 1);
    assert!(check_id(17, 18, 1, 2).is_err());
    assert!(check_id(17, 17, 2, 2).is_err());
    assert!(check_id(17, 17, 0, 0).is_err());
}

#[test]
fn buffer_dependencies_follow_overlap_and_writes_not_copy_count() {
    let access = |resource, range, write| BufferAccess {
        resource,
        range,
        write,
    };
    let mut hazards = BufferHazards::default();
    assert!(!hazards.before(&[access(0, 0..64, true)]));
    assert!(!hazards.before(&[access(1, 0..64, true)]));
    assert!(!hazards.before(&[access(0, 64..128, true)]));
    assert!(hazards.before(&[access(0, 16..32, false)]));
    // The inserted global dependency also retires prior writes to resource 1.
    assert!(!hazards.before(&[access(1, 0..64, false)]));
    assert!(!hazards.before(&[access(0, 16..32, false)]));
    assert!(hazards.before(&[access(0, 16..20, true)]));
    assert!(hazards.before(&[access(0, 18..24, true)]));
}

#[test]
fn image_transitions_cover_initial_upload_repeated_writes_and_readback() {
    for (old, new, source_stage, source_access, destination_access) in [
        (
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
            vk::AccessFlags2::TRANSFER_WRITE,
        ),
        (
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_WRITE,
            vk::AccessFlags2::TRANSFER_WRITE,
        ),
        (
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_WRITE,
            vk::AccessFlags2::TRANSFER_READ,
        ),
        (
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
            vk::AccessFlags2::TRANSFER_WRITE,
        ),
    ] {
        let barrier = image_barrier(vk::Image::null(), old, new);
        assert_eq!((barrier.old_layout, barrier.new_layout), (old, new));
        assert_eq!(barrier.src_stage_mask, source_stage);
        assert_eq!(barrier.src_access_mask, source_access);
        assert_eq!(barrier.dst_stage_mask, vk::PipelineStageFlags2::COPY);
        assert_eq!(barrier.dst_access_mask, destination_access);
        assert_eq!(barrier.src_queue_family_index, vk::QUEUE_FAMILY_IGNORED);
        assert_eq!(barrier.dst_queue_family_index, vk::QUEUE_FAMILY_IGNORED);
        assert_eq!(
            barrier.subresource_range.aspect_mask,
            vk::ImageAspectFlags::COLOR
        );
        assert_eq!(barrier.subresource_range.level_count, 1);
        assert_eq!(barrier.subresource_range.layer_count, 1);
    }
}

fn pattern(size: usize, seed: usize) -> Vec<u8> {
    (0..size)
        .map(|index| (index * 37 + (index / 17) * 13 + seed) as u8)
        .collect()
}

fn finish(mut pending: super::super::PendingTransfer<TransferBatch>) -> TransferBatch {
    pending
        .wait(Duration::ZERO)
        .unwrap()
        .or_else(|| pending.wait(Duration::from_secs(10)).unwrap())
        .expect("copy batch completed within finite wait")
}

fn cancellation_and_inputs(allocator: &VulkanAllocator, pool: &TransferPool) {
    let mut first = pool.begin().unwrap();
    let id = first
        .add_buffer(
            allocator
                .create_buffer(BufferRole::DeviceTransfer, 512)
                .unwrap(),
        )
        .unwrap();
    assert!(first.readback_buffer(id, 0, 4).is_err());
    assert!(first.upload_buffer(id, 2, &[1, 2, 3, 4]).is_err());
    assert!(first.upload_buffer(id, 0, &[]).is_err());
    assert!(first.upload_buffer(id, 508, &[0; 8]).is_err());
    let image = first
        .add_image(allocator.create_image(ImageDesc::transfer(1, 1)).unwrap())
        .unwrap();
    assert!(first.readback_image(image).is_err());
    assert!(first.upload_image(image, &[1, 2, 3]).is_err());
    let mut second = pool.begin().unwrap();
    second
        .add_buffer(
            allocator
                .create_buffer(BufferRole::DeviceTransfer, 512)
                .unwrap(),
        )
        .unwrap();
    assert!(second.upload_buffer(id, 0, &[1, 2, 3, 4]).is_err());
    assert!(matches!(
        pool.begin(),
        Err(BatchError::Execution(
            TransferError::CapacityExhausted { .. }
        ))
    ));
    drop(first);
    drop(second);
    let (upload, readback) = pool.staging_stats();
    assert_eq!((upload.live_ranges, readback.live_ranges), (0, 0));
    assert_eq!(pool.execution_stats().leased, 0);
    for record in [false, true] {
        let mut cancel = pool.begin().unwrap();
        let buffer = cancel
            .add_buffer(
                allocator
                    .create_buffer(BufferRole::DeviceTransfer, 64)
                    .unwrap(),
            )
            .unwrap();
        cancel.upload_buffer(buffer, 0, &[19; 64]).unwrap();
        cancel.readback_buffer(buffer, 0, 64).unwrap();
        assert_eq!(pool.staging_stats().0.live_ranges, 1);
        assert_eq!(pool.staging_stats().1.live_ranges, 1);
        if record {
            drop(cancel.record().unwrap());
        } else {
            drop(cancel);
        }
        assert_eq!(pool.staging_stats().0.live_ranges, 0);
        assert_eq!(pool.staging_stats().1.live_ranges, 0);
        assert_eq!(pool.execution_stats().leased, 0);
    }
}

fn independent_batches(allocator: &VulkanAllocator, pool: &TransferPool, iteration: usize) {
    let bytes = pattern(257, iteration * 19);
    let pixels = pattern(17 * 13 * 4, iteration * 23);
    let mut first = pool.begin().unwrap();
    let source = first
        .add_buffer(
            allocator
                .create_buffer(BufferRole::DeviceTransfer, 512)
                .unwrap(),
        )
        .unwrap();
    let target = first
        .add_buffer(
            allocator
                .create_buffer(BufferRole::DeviceTransfer, 512)
                .unwrap(),
        )
        .unwrap();
    first.upload_buffer(source, 0, &bytes).unwrap();
    first
        .copy_buffer(source, 0, target, 4, bytes.len() as u64)
        .unwrap();
    assert!(first.readback_buffer(target, 0, 4).is_err());
    let buffer_result = first
        .readback_buffer(target, 4, bytes.len() as u64)
        .unwrap();
    let image = first
        .add_image(allocator.create_image(ImageDesc::transfer(17, 13)).unwrap())
        .unwrap();
    first.upload_image(image, &pixels).unwrap();
    let image_result = first.readback_image(image).unwrap();
    assert!(first.work.uploads[1].offset() > 0);
    assert!(
        first.work.readbacks[image_result.index]
            .as_ref()
            .unwrap()
            .offset()
            > 0
    );
    assert!(
        !first.work.readbacks[image_result.index]
            .as_ref()
            .unwrap()
            .initialized()
    );
    let first_pending = first.record().unwrap().submit().unwrap();

    // Host writes to different staging ranges are permitted while the first
    // batch retains its GPU ranges. No first-batch completion wait occurs here.
    let second_bytes = pattern(64, iteration * 29 + 7);
    let second_pixel = pattern(4, iteration * 31 + 11);
    let mut second = pool.begin().unwrap();
    let second_buffer = second
        .add_buffer(
            allocator
                .create_buffer(BufferRole::DeviceTransfer, 64)
                .unwrap(),
        )
        .unwrap();
    second
        .upload_buffer(second_buffer, 0, &second_bytes)
        .unwrap();
    let second_buffer_result = second.readback_buffer(second_buffer, 0, 64).unwrap();
    let second_image = second
        .add_image(allocator.create_image(ImageDesc::transfer(1, 1)).unwrap())
        .unwrap();
    second.upload_image(second_image, &second_pixel).unwrap();
    let second_image_result = second.readback_image(second_image).unwrap();
    assert!(second.work.uploads[0].offset() > 0);
    let second_pending = second.record().unwrap().submit().unwrap();
    assert!(matches!(
        pool.begin(),
        Err(BatchError::Execution(
            TransferError::CapacityExhausted { .. }
        ))
    ));

    let mut first_done = finish(first_pending);
    assert_eq!(pool.execution_stats().leased, 1);
    assert_eq!(pool.staging_stats().1.live_ranges, 4);
    // Native resources are available even though this completed result still
    // owns its readback ranges; a temporary reservation must not reclaim them.
    let spare = pool.begin().unwrap();
    assert_eq!(pool.staging_stats().1.live_ranges, 4);
    drop(spare);
    // Read one completed batch while the other pending owner still retains
    // different regions in the same native staging buffers.
    let mut output = vec![0; bytes.len()];
    assert!(
        first_done
            .readback(buffer_result, &mut output[..1])
            .is_err()
    );
    assert_eq!(pool.staging_stats().1.live_ranges, 4);
    first_done.readback(buffer_result, &mut output).unwrap();
    assert_eq!(output, bytes);
    assert!(first_done.readback(buffer_result, &mut output).is_err());
    let mut image_output = vec![0; pixels.len()];
    first_done
        .readback(image_result, &mut image_output)
        .unwrap();
    assert_eq!(image_output, pixels);
    assert_eq!(pool.staging_stats().1.live_ranges, 2);
    let mut second_done = finish(second_pending);
    assert_eq!(pool.execution_stats().leased, 0);
    assert_eq!(pool.staging_stats().0.live_ranges, 0);
    assert_eq!(pool.staging_stats().1.live_ranges, 2);
    let mut second_output = vec![0; second_bytes.len()];
    second_done
        .readback(second_buffer_result, &mut second_output)
        .unwrap();
    assert_eq!(second_output, second_bytes);
    let mut pixel_output = [0; 4];
    second_done
        .readback(second_image_result, &mut pixel_output)
        .unwrap();
    assert_eq!(pixel_output.as_slice(), second_pixel);
    assert_eq!(pool.staging_stats().1.live_ranges, 0);

    // Prior initialized contents and a new overlapping write must combine into
    // the source range; the unwritten suffix must remain uninitialized.
    let source_buffer = first_done.take_buffer(source).unwrap();
    let target_buffer = first_done.take_buffer(target).unwrap();
    let mut merge = pool.begin().unwrap();
    let source = merge.add_buffer(source_buffer).unwrap();
    let target = merge.add_buffer(target_buffer).unwrap();
    let tail = [91, 92, 93, 94];
    merge.upload_buffer(source, 256, &tail).unwrap();
    assert!(merge.readback_buffer(source, 0, 264).is_err());
    merge.copy_buffer(source, 0, target, 0, 260).unwrap();
    let merged_result = merge.readback_buffer(target, 0, 260).unwrap();
    let mut merged = finish(merge.record().unwrap().submit().unwrap());
    let mut merged_output = vec![0; 260];
    merged.readback(merged_result, &mut merged_output).unwrap();
    let mut expected = bytes[..256].to_vec();
    expected.extend_from_slice(&tail);
    assert_eq!(merged_output, expected);

    // Upload-only and later readback-only preserve the same native image and
    // establish their final layout only after their actual execution finishes.
    let image = first_done.take_image(image).unwrap();
    assert_eq!(image.layout(), vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
    let new_pixels = pattern(pixels.len(), iteration * 41 + 103);
    let mut upload = pool.begin().unwrap();
    let image = upload.add_image(image).unwrap();
    upload.upload_image(image, &new_pixels).unwrap();
    assert!(upload.work.readbacks.is_empty());
    let mut uploaded = finish(upload.record().unwrap().submit().unwrap());
    let image = uploaded.take_image(image).unwrap();
    assert_eq!(image.layout(), vk::ImageLayout::TRANSFER_DST_OPTIMAL);
    let mut read = pool.begin().unwrap();
    let image = read.add_image(image).unwrap();
    let result = read.readback_image(image).unwrap();
    assert!(read.work.uploads.is_empty());
    let mut read = finish(read.record().unwrap().submit().unwrap());
    let mut new_output = vec![0; new_pixels.len()];
    read.readback(result, &mut new_output).unwrap();
    assert_eq!(new_output, new_pixels);
    assert_eq!(
        read.take_image(image).unwrap().layout(),
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL
    );
    assert_eq!(pool.staging_stats().0.live_ranges, 0);
    assert_eq!(pool.staging_stats().1.live_ranges, 0);
}

fn held_readback_limits_staging_not_native(
    allocator: &VulkanAllocator,
    pool: &TransferPool,
    iteration: usize,
) {
    let capacity = pool.staging_stats().1.capacity;
    let bytes = pattern(capacity as usize, iteration + 211);
    let mut full = pool.begin().unwrap();
    let buffer = full
        .add_buffer(
            allocator
                .create_buffer(BufferRole::DeviceTransfer, capacity)
                .unwrap(),
        )
        .unwrap();
    full.upload_buffer(buffer, 0, &bytes).unwrap();
    let result = full.readback_buffer(buffer, 0, capacity).unwrap();
    let mut done = finish(full.record().unwrap().submit().unwrap());
    assert_eq!(pool.execution_stats().leased, 0);
    assert_eq!(pool.staging_stats().0.live_ranges, 0);
    assert_eq!(pool.staging_stats().1.reserved_bytes, capacity);
    let mut next = pool.begin().unwrap();
    let buffer = next.add_buffer(done.take_buffer(buffer).unwrap()).unwrap();
    assert!(matches!(
        next.readback_buffer(buffer, 0, capacity),
        Err(BatchError::Staging(StagingError::CapacityExhausted { .. }))
    ));
    let mut output = vec![0; bytes.len()];
    done.readback(result, &mut output).unwrap();
    assert_eq!(output, bytes);
    let result = next.readback_buffer(buffer, 0, capacity).unwrap();
    assert!(
        !next.work.readbacks[result.index]
            .as_ref()
            .unwrap()
            .initialized()
    );
    output.fill(73);
    assert!(matches!(
        next.work.readbacks[result.index]
            .as_mut()
            .unwrap()
            .read(&mut output),
        Err(StagingError::Uninitialized)
    ));
    assert!(output.iter().all(|byte| *byte == 73));
    let mut next = finish(next.record().unwrap().submit().unwrap());
    next.readback(result, &mut output).unwrap();
    assert_eq!(output, bytes);
    assert_eq!(pool.staging_stats().1.live_ranges, 0);
}

fn repeated_uploads_preserve_each_readback(
    allocator: &VulkanAllocator,
    pool: &TransferPool,
    iteration: usize,
) {
    let before = pattern(64, iteration * 43 + 17);
    let after = pattern(64, iteration * 47 + 131);
    let mut batch = pool.begin().unwrap();
    let buffer = batch
        .add_buffer(
            allocator
                .create_buffer(BufferRole::DeviceTransfer, 64)
                .unwrap(),
        )
        .unwrap();
    batch.upload_buffer(buffer, 0, &before).unwrap();
    let before_buffer = batch.readback_buffer(buffer, 0, 64).unwrap();
    batch.upload_buffer(buffer, 0, &after).unwrap();
    let after_buffer = batch.readback_buffer(buffer, 0, 64).unwrap();
    let image = batch
        .add_image(allocator.create_image(ImageDesc::transfer(1, 1)).unwrap())
        .unwrap();
    batch.upload_image(image, &before[..4]).unwrap();
    let before_image = batch.readback_image(image).unwrap();
    batch.upload_image(image, &after[..4]).unwrap();
    let after_image = batch.readback_image(image).unwrap();
    // One recording/submit must order reads before later overlapping writes;
    // every readback region preserves the version at its position in the batch.
    let mut done = finish(batch.record().unwrap().submit().unwrap());
    for (result, expected) in [
        (before_buffer, before.as_slice()),
        (after_buffer, after.as_slice()),
        (before_image, &before[..4]),
        (after_image, &after[..4]),
    ] {
        let mut output = vec![0; expected.len()];
        done.readback(result, &mut output).unwrap();
        assert_eq!(output, expected);
    }
    assert_eq!(pool.staging_stats().0.live_ranges, 0);
    assert_eq!(pool.staging_stats().1.live_ranges, 0);
}

#[test]
#[ignore = "explicit shared-staging GPU batch test; requires validation, SyncVal and debug_utils"]
fn vulkan_batch_smoke() {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        for iteration in 0..2 {
            let diagnostics = Diagnostics::new();
            let instance = VulkanInstance::new(Arc::clone(&diagnostics)).unwrap();
            crate::backends::vulkan::assert_real_validation(&diagnostics);
            let device = VulkanDevice::new(instance).unwrap();
            assert_ne!(
                device.device_type(),
                vk::PhysicalDeviceType::CPU,
                "CPU fallback does not count as real-GPU acceptance"
            );
            // SAFETY: The live Device retains this physical device and Instance;
            // the immutable limits query requires no Device host synchronization.
            let limits = unsafe {
                device
                    .instance_raw()
                    .get_physical_device_properties(device.physical_device())
            }
            .limits;
            let capacity = limits.non_coherent_atom_size.max(1024) * 64;
            let allocator = VulkanAllocator::new(device).unwrap();
            let pool =
                TransferPool::new(Arc::clone(&allocator.owner), 2, capacity, capacity).unwrap();
            cancellation_and_inputs(&allocator, &pool);
            independent_batches(&allocator, &pool, iteration);
            repeated_uploads_preserve_each_readback(&allocator, &pool, iteration);
            held_readback_limits_staging_not_native(&allocator, &pool, iteration);
            assert_eq!(pool.execution_stats().native_creates, 2);
            assert!(pool.execution_stats().reuses > 0);
            assert_eq!(pool.staging_stats().0.buffer_creates, 1);
            assert_eq!(pool.staging_stats().1.buffer_creates, 1);

            let lifetime = Arc::downgrade(&allocator.owner);
            let mut last = pool.begin().unwrap();
            let buffer = last
                .add_buffer(
                    allocator
                        .create_buffer(BufferRole::DeviceTransfer, 64)
                        .unwrap(),
                )
                .unwrap();
            last.upload_buffer(buffer, 0, &[iteration as u8; 64])
                .unwrap();
            last.readback_buffer(buffer, 0, 64).unwrap();
            let pending = last.record().unwrap().submit().unwrap();
            drop(pool);
            drop(allocator);
            assert!(lifetime.upgrade().is_some());
            drop(pending);
            assert!(
                lifetime.upgrade().is_none(),
                "last GPU owner retained an unexpected Arc cycle"
            );
            let report = diagnostics.snapshot();
            assert!(report.is_clean(), "including final shutdown: {report:#?}");
            tracing::info!(target: LOG_TARGET, iteration, warnings = report.warnings,
                errors = report.errors, callback_failures = report.callback_failures,
                "Shared staging batches, range retirement and shutdown passed");
        }
    });
}

use std::sync::Arc;
use std::time::Duration;

use ash::vk;

use super::super::super::buffer::{BufferError, BufferRole};
use super::super::super::device::VulkanDevice;
use super::super::super::diagnostics::Diagnostics;
use super::super::super::image::ImageDesc;
use super::super::super::instance::{LOG_TARGET, VulkanInstance};
use super::super::super::memory::VulkanAllocator;
use super::super::{BufferSpec, RecordedTransfer, TransferError};
use super::{ImageTransfer, validate_request};

fn specs() -> [BufferSpec; 2] {
    [
        BufferSpec {
            role: BufferRole::Upload,
            size: 17 * 13 * 4,
            initialized: true,
        },
        BufferSpec {
            role: BufferRole::Readback,
            size: 17 * 13 * 4,
            initialized: false,
        },
    ]
}

#[test]
fn image_transfer_rejects_invalid_ownership_roles_packing_and_state() {
    let desc = ImageDesc::transfer(17, 13);
    let layout = vk::ImageLayout::UNDEFINED;
    validate_request(desc, layout, specs(), true).unwrap();
    assert!(validate_request(desc, layout, specs(), false).is_err());
    for index in 0..2 {
        let mut wrong_role = specs();
        wrong_role[index].role = BufferRole::DeviceTransfer;
        assert!(validate_request(desc, layout, wrong_role, true).is_err());
        for size in [0, 883, 885] {
            let mut wrong_size = specs();
            wrong_size[index].size = size;
            assert!(validate_request(desc, layout, wrong_size, true).is_err());
        }
    }
    let mut uninitialized = specs();
    uninitialized[0].initialized = false;
    assert!(validate_request(desc, layout, uninitialized, true).is_err());
    for invalid in [ImageDesc::transfer(0, 13), ImageDesc::transfer(17, 0)] {
        assert!(validate_request(invalid, layout, specs(), true).is_err());
    }
    assert!(matches!(
        validate_request(
            ImageDesc::transfer(u32::MAX, u32::MAX),
            layout,
            specs(),
            true
        ),
        Err(TransferError::InvalidImage(
            "packed RGBA8 byte count overflows DeviceSize"
        ))
    ));
    let mut unsupported = desc;
    unsupported.format = vk::Format::R8_UNORM;
    assert!(validate_request(unsupported, layout, specs(), true).is_err());
    unsupported = desc;
    unsupported.usage |= vk::ImageUsageFlags::SAMPLED;
    assert!(validate_request(unsupported, layout, specs(), true).is_err());
    assert!(validate_request(desc, vk::ImageLayout::TRANSFER_DST_OPTIMAL, specs(), true).is_err());
}

#[test]
fn image_copy_plan_preserves_completed_layout_and_packed_addressing() {
    let desc = ImageDesc::transfer(17, 13);
    for layout in [
        vk::ImageLayout::UNDEFINED,
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
    ] {
        let plan = validate_request(desc, layout, specs(), true).unwrap();
        let region = plan.region();
        assert_eq!(
            (
                region.buffer_offset,
                region.buffer_row_length,
                region.buffer_image_height
            ),
            (0, 0, 0)
        );
        assert_eq!(
            region.image_extent,
            vk::Extent3D {
                width: 17,
                height: 13,
                depth: 1
            }
        );
        assert_eq!(region.image_offset, vk::Offset3D::default());
        assert_eq!(
            region.image_subresource.aspect_mask,
            vk::ImageAspectFlags::COLOR
        );
        assert_eq!(
            (
                region.image_subresource.mip_level,
                region.image_subresource.base_array_layer,
                region.image_subresource.layer_count
            ),
            (0, 0, 1)
        );
        let destination = plan.to_destination(vk::Image::null());
        assert_eq!(destination.old_layout, layout);
        assert_eq!(
            destination.new_layout,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL
        );
        assert_eq!(
            destination.src_stage_mask,
            if layout == vk::ImageLayout::UNDEFINED {
                vk::PipelineStageFlags2::NONE
            } else {
                vk::PipelineStageFlags2::COPY
            }
        );
        assert_eq!(
            destination.src_access_mask,
            if layout == vk::ImageLayout::UNDEFINED {
                vk::AccessFlags2::NONE
            } else {
                vk::AccessFlags2::TRANSFER_READ
            }
        );
        assert_eq!(destination.dst_stage_mask, vk::PipelineStageFlags2::COPY);
        assert_eq!(
            destination.dst_access_mask,
            vk::AccessFlags2::TRANSFER_WRITE
        );
        let source = plan.to_source(vk::Image::null());
        assert_eq!(
            (source.old_layout, source.new_layout),
            (
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL
            )
        );
        assert_eq!(
            (source.src_stage_mask, source.dst_stage_mask),
            (vk::PipelineStageFlags2::COPY, vk::PipelineStageFlags2::COPY)
        );
        assert_eq!(
            (source.src_access_mask, source.dst_access_mask),
            (
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::AccessFlags2::TRANSFER_READ
            )
        );
        for barrier in [destination, source] {
            assert_eq!(
                (
                    barrier.src_queue_family_index,
                    barrier.dst_queue_family_index
                ),
                (vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED)
            );
            assert_eq!(
                barrier.subresource_range.aspect_mask,
                vk::ImageAspectFlags::COLOR
            );
            assert_eq!(
                (
                    barrier.subresource_range.base_mip_level,
                    barrier.subresource_range.level_count,
                    barrier.subresource_range.base_array_layer,
                    barrier.subresource_range.layer_count
                ),
                (0, 1, 0, 1)
            );
        }
    }
}

fn resources(allocator: &VulkanAllocator, width: u32, height: u32) -> ImageTransfer {
    let bytes = u64::from(width) * u64::from(height) * 4;
    ImageTransfer {
        upload: allocator.create_buffer(BufferRole::Upload, bytes).unwrap(),
        image: allocator
            .create_image(ImageDesc::transfer(width, height))
            .unwrap(),
        readback: allocator
            .create_buffer(BufferRole::Readback, bytes)
            .unwrap(),
    }
}

fn pixels(width: u32, height: u32, iteration: u32, pass: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    for y in 0..height {
        for x in 0..width {
            for channel in 0..4 {
                bytes.push((x * 13 + y * 47 + channel * 71 + pass * 29 + iteration) as u8);
            }
        }
    }
    bytes
}

#[test]
#[ignore = "explicit real-GPU image transfer test; requires validation, SyncVal and debug_utils"]
fn vulkan_image_transfer_smoke() {
    let dispatch = super::super::super::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        for iteration in 0..4 {
            let diagnostics = Diagnostics::new();
            let instance = VulkanInstance::new(Arc::clone(&diagnostics)).unwrap();
            super::super::super::assert_real_validation(&diagnostics);
            let device = VulkanDevice::new(instance).unwrap();
            assert_ne!(
                device.device_type(),
                vk::PhysicalDeviceType::CPU,
                "CPU fallback does not count as real-GPU acceptance"
            );
            let allocator = VulkanAllocator::new(device).unwrap();
            let lifetime = Arc::downgrade(&allocator.owner);

            // Cancel a valid recording before submission; no GPU wait is needed.
            let mut canceled = resources(&allocator, 17, 13);
            canceled
                .upload
                .write(0, &pixels(17, 13, iteration, 0))
                .unwrap();
            drop(RecordedTransfer::new(canceled).unwrap());

            for (width, height) in [(1, 1), (17, 13), (257, 3)] {
                let mut reused = resources(&allocator, width, height);
                assert_eq!(reused.image.layout(), vk::ImageLayout::UNDEFINED);
                let mut output = vec![0; (width * height * 4) as usize];
                assert!(matches!(
                    reused.readback.read(0, &mut output),
                    Err(BufferError::Uninitialized { .. })
                ));
                for pass in 0..3 {
                    let expected = pixels(width, height, iteration, pass);
                    reused.upload.write(0, &expected).unwrap();
                    let mut pending = RecordedTransfer::new(reused).unwrap().submit().unwrap();
                    // A zero-time wait may observe either completion or timeout.
                    // Only successful completion returns the retained resources.
                    reused = match pending.wait(Duration::ZERO).unwrap() {
                        Some(done) => done,
                        None => pending
                            .wait(Duration::from_secs(10))
                            .unwrap()
                            .expect("tiny image transfer must complete within test budget"),
                    };
                    assert_eq!(reused.image.layout(), vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
                    reused.readback.read(0, &mut output).unwrap();
                    assert_eq!(
                        output, expected,
                        "iteration {iteration}, {width}x{height}, pass {pass}"
                    );
                    assert!(matches!(
                        pending.wait(Duration::ZERO),
                        Err(TransferError::AlreadyFinished)
                    ));
                    tracing::info!(target: LOG_TARGET, iteration, width, height, pass,
                        bytes = output.len(), "GPU image transfer readback matched every byte");
                }
            }

            let mut abandoned = resources(&allocator, 17, 13);
            abandoned
                .upload
                .write(0, &pixels(17, 13, iteration, 4))
                .unwrap();
            let pending = RecordedTransfer::new(abandoned).unwrap().submit().unwrap();
            drop(allocator);
            assert!(lifetime.upgrade().is_some());
            // Even when work already finished on the GPU, this pending owner
            // must obtain its Fence result before releasing the retained chain.
            drop(pending);
            assert!(lifetime.upgrade().is_none());
            let report = diagnostics.snapshot();
            assert!(
                report.is_clean(),
                "iteration {iteration}, including shutdown: {report:#?}"
            );
            tracing::info!(target: LOG_TARGET, iteration, warnings = report.warnings,
                errors = report.errors, callback_failures = report.callback_failures,
                "Vulkan image transfer smoke including cancellation, reuse and Drop wait passed");
        }
    });
}

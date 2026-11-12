//! Whole-image RGBA8 copies with completed, single-subresource layout tracking.

use std::ffi::CStr;
use std::sync::Arc;

use ash::vk;

use super::super::buffer::BufferRole;
use super::super::image::ImageDesc;
use super::super::instance::LOG_TARGET;
use super::super::memory::MemoryOwner;
use super::{BufferSpec, ImageTransfer, TransferError, TransferPayload, record_host_read};

struct CopyPlan {
    extent: vk::Extent3D,
    old_layout: vk::ImageLayout,
}

fn validate_request(
    desc: ImageDesc,
    layout: vk::ImageLayout,
    buffers: [BufferSpec; 2],
    same_owner: bool,
) -> Result<CopyPlan, TransferError> {
    let invalid = TransferError::InvalidImage;
    if !same_owner {
        return Err(invalid(
            "image and buffers must belong to the same Allocator and Device",
        ));
    }
    if desc.extent.width == 0 || desc.extent.height == 0 {
        return Err(invalid("image width and height must be nonzero"));
    }
    if desc != ImageDesc::transfer(desc.extent.width, desc.extent.height) {
        return Err(invalid(
            "expected an RGBA8_UNORM transfer source and destination image",
        ));
    }
    let bytes = u64::from(desc.extent.width)
        .checked_mul(u64::from(desc.extent.height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(invalid("packed RGBA8 byte count overflows DeviceSize"))?;
    if buffers[0].role != BufferRole::Upload || buffers[1].role != BufferRole::Readback {
        return Err(invalid("expected Upload and Readback buffer roles"));
    }
    if buffers.iter().any(|buffer| buffer.size != bytes) {
        return Err(invalid(
            "buffer sizes must equal width * height * 4, without row padding",
        ));
    }
    if !buffers[0].initialized {
        return Err(invalid("Upload contents are not fully initialized"));
    }
    if !matches!(
        layout,
        vk::ImageLayout::UNDEFINED | vk::ImageLayout::TRANSFER_SRC_OPTIMAL
    ) {
        return Err(invalid(
            "image must be fresh or returned by a completed image transfer",
        ));
    }
    Ok(CopyPlan {
        extent: vk::Extent3D {
            width: desc.extent.width,
            height: desc.extent.height,
            depth: 1,
        },
        old_layout: layout,
    })
}

fn subresources() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
}

impl CopyPlan {
    fn region(&self) -> vk::BufferImageCopy {
        vk::BufferImageCopy::default()
            .buffer_offset(0)
            .buffer_row_length(0)
            .buffer_image_height(0)
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(1),
            )
            .image_offset(vk::Offset3D::default())
            .image_extent(self.extent)
    }

    fn to_destination(&self, image: vk::Image) -> vk::ImageMemoryBarrier2<'static> {
        let reused = self.old_layout == vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
        vk::ImageMemoryBarrier2::default()
            .src_stage_mask(if reused {
                vk::PipelineStageFlags2::COPY
            } else {
                vk::PipelineStageFlags2::NONE
            })
            .src_access_mask(if reused {
                vk::AccessFlags2::TRANSFER_READ
            } else {
                vk::AccessFlags2::NONE
            })
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(self.old_layout)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(subresources())
    }

    fn to_source(&self, image: vk::Image) -> vk::ImageMemoryBarrier2<'static> {
        vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(subresources())
    }
}

impl ImageTransfer {
    fn plan(&self) -> Result<CopyPlan, TransferError> {
        validate_request(
            self.image.desc(),
            self.image.layout(),
            [&self.upload, &self.readback].map(|buffer| BufferSpec {
                role: buffer.role(),
                size: buffer.size(),
                initialized: buffer.fully_initialized(),
            }),
            Arc::ptr_eq(self.upload.owner(), self.image.owner())
                && Arc::ptr_eq(self.upload.owner(), self.readback.owner()),
        )
    }
}

impl TransferPayload for ImageTransfer {
    fn validate(&self) -> Result<(), TransferError> {
        self.plan().map(|_| ())
    }

    fn owner(&self) -> &Arc<MemoryOwner> {
        self.upload.owner()
    }

    fn bytes(&self) -> u64 {
        self.upload.size()
    }

    fn label(&self) -> &'static CStr {
        c"renderer upload -> image -> readback"
    }

    unsafe fn record(&self, command: vk::CommandBuffer) {
        let plan = self.plan().expect("payload validated before recording");
        let device = &self.owner().device;
        // SAFETY: The caller exclusively retains this validated payload through
        // GPU completion; the raw image cannot escape that ownership boundary.
        let image = unsafe { self.image.transfer_handle() };
        let initial = [plan.to_destination(image)];
        let between = [plan.to_source(image)];
        let region = [plan.region()];
        // SAFETY: Every resource belongs to this live Device and remains retained
        // by the recording owner. The completed old layout is preserved until
        // the whole copy succeeds. Both barriers cover the sole color mip/layer;
        // COPY write->read visibility protects readback. Packed buffer addressing
        // describes CPU bytes, not the optimal image's native allocation pitch.
        unsafe {
            device.raw().cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().image_memory_barriers(&initial),
            );
            device.raw().cmd_copy_buffer_to_image(
                command,
                self.upload.transfer_handle(),
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &region,
            );
            device.raw().cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().image_memory_barriers(&between),
            );
            device.raw().cmd_copy_image_to_buffer(
                command,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.readback.transfer_handle(),
                &region,
            );
            record_host_read(device, command);
        }
        tracing::debug!(target: LOG_TARGET, device = device.name(), ?image, ?command,
            width = plan.extent.width, height = plan.extent.height, bytes = self.bytes(),
            old_layout = ?plan.old_layout, final_layout = ?vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            "Vulkan image transfer commands recorded");
    }

    unsafe fn establish_contents(&mut self) {
        // SAFETY: The caller proved full-image upload, layout transitions and
        // full-buffer readback completed on the active Device via their Fence.
        unsafe {
            self.image.establish_transfer_contents();
            self.readback.establish_gpu_contents();
        }
    }
}

#[cfg(test)]
mod tests;

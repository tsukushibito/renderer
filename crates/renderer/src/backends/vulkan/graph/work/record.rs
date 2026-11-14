//! Native command recording and completion for an already prepared Graph.

use std::ffi::CStr;
use std::sync::Arc;

use ash::vk;

use super::super::super::memory::MemoryOwner;
use super::super::super::resources::DeviceImage;
use super::super::super::transfer::staging::StagingRange;
use super::super::super::transfer::{TransferError, TransferPayload};
use super::{ComputeTransform, GraphWork, PreparedOperation};
use crate::graph::{LoadAction, StoreOp};

impl TransferPayload for GraphWork {
    fn validate(&self) -> Result<(), TransferError> {
        self.owner
            .device
            .ensure_active()
            .map_err(TransferError::DeviceUnavailable)
    }
    fn owner(&self) -> &Arc<MemoryOwner> {
        &self.owner
    }
    fn bytes(&self) -> u64 {
        self.uploads
            .iter()
            .chain(
                self.readbacks
                    .iter()
                    .filter_map(|(_, range)| range.as_ref()),
            )
            .map(StagingRange::size)
            .sum()
    }
    fn label(&self) -> &'static CStr {
        c"renderer.graph"
    }

    unsafe fn record(&self, command: vk::CommandBuffer) {
        let device = &self.owner.device;
        if let Some(lease) = self
            .profile
            .as_ref()
            .and_then(|profile| profile.lease.as_ref())
        {
            // SAFETY: This command and unique query pair are retained by this
            // recording/pending payload, outside all dynamic rendering scopes.
            unsafe { lease.begin(command) };
        }
        for pass in &self.passes {
            // SAFETY: Every command operand is validated and retained in this
            // payload. Pass boundaries lie outside all dynamic rendering scopes.
            unsafe {
                #[cfg(feature = "vulkan-diagnostics")]
                device.debug_utils().cmd_begin_debug_utils_label(
                    command,
                    &vk::DebugUtilsLabelEXT::default().label_name(&pass.name),
                );
                if !pass.buffers.is_empty() || !pass.images.is_empty() {
                    device.raw().cmd_pipeline_barrier2(
                        command,
                        &vk::DependencyInfo::default()
                            .buffer_memory_barriers(&pass.buffers)
                            .image_memory_barriers(&pass.images),
                    );
                }
                match &pass.operation {
                    PreparedOperation::UploadBuffer { dst, offset, size } => {
                        let range = &self.uploads[pass.staging.unwrap()];
                        debug_assert_eq!(range.size(), *size);
                        device.raw().cmd_copy_buffer(
                            command,
                            range.transfer_handle(),
                            self.resource(*dst).buffer().handle,
                            &[vk::BufferCopy::default()
                                .src_offset(range.offset())
                                .dst_offset(*offset)
                                .size(*size)],
                        );
                    }
                    PreparedOperation::CopyBuffer {
                        src,
                        dst,
                        src_offset,
                        dst_offset,
                        size,
                    } => device.raw().cmd_copy_buffer(
                        command,
                        self.resource(*src).buffer().handle,
                        self.resource(*dst).buffer().handle,
                        &[vk::BufferCopy::default()
                            .src_offset(*src_offset)
                            .dst_offset(*dst_offset)
                            .size(*size)],
                    ),
                    PreparedOperation::ReadbackBuffer { src, offset, size } => {
                        let range = self.readbacks[pass.staging.unwrap()].1.as_ref().unwrap();
                        device.raw().cmd_copy_buffer(
                            command,
                            self.resource(*src).buffer().handle,
                            range.transfer_handle(),
                            &[vk::BufferCopy::default()
                                .src_offset(*offset)
                                .dst_offset(range.offset())
                                .size(*size)],
                        );
                    }
                    PreparedOperation::UploadImage { dst, .. } => {
                        let range = &self.uploads[pass.staging.unwrap()];
                        let image = self.resource(*dst).image();
                        device.raw().cmd_copy_buffer_to_image(
                            command,
                            range.transfer_handle(),
                            image.image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[image_copy(image, range.offset())],
                        );
                    }
                    PreparedOperation::CopyImage { src, dst } => {
                        let source = self.resource(*src).image();
                        let target = self.resource(*dst).image();
                        device.raw().cmd_copy_image(
                            command,
                            source.image,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            target.image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[vk::ImageCopy::default()
                                .src_subresource(layers(source))
                                .dst_subresource(layers(target))
                                .extent(vk::Extent3D {
                                    width: source.extent.width,
                                    height: source.extent.height,
                                    depth: 1,
                                })],
                        );
                    }
                    PreparedOperation::ReadbackImage { src } => {
                        let range = self.readbacks[pass.staging.unwrap()].1.as_ref().unwrap();
                        let image = self.resource(*src).image();
                        device.raw().cmd_copy_image_to_buffer(
                            command,
                            image.image,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            range.transfer_handle(),
                            &[image_copy(image, range.offset())],
                        );
                    }
                    PreparedOperation::Compute { dst, transform, .. } => {
                        self.programs.as_ref().unwrap().record_compute(
                            command,
                            pass.descriptor.unwrap(),
                            self.resource(*dst).image(),
                            match transform {
                                ComputeTransform::Invert => 0,
                                ComputeTransform::SwapRedBlue => 1,
                            },
                        )
                    }
                    PreparedOperation::Raster {
                        vertices,
                        indices,
                        index_count,
                        color,
                        depth,
                        color_ops,
                        depth_ops,
                        clear_color,
                        clear_depth,
                        parameters,
                    } => self.programs.as_ref().unwrap().record_raster(
                        command,
                        self.resource(*vertices).buffer().handle,
                        self.resource(*indices).buffer().handle,
                        *index_count,
                        self.resource(*color).image(),
                        self.resource(*depth).image(),
                        load(color_ops.load),
                        store(color_ops.store),
                        *clear_color,
                        load(depth_ops.load),
                        store(depth_ops.store),
                        *clear_depth,
                        parameters,
                    ),
                    PreparedOperation::RasterBatch {
                        draws,
                        color,
                        depth,
                        clear_color,
                        clear_depth,
                    } => self.programs.as_ref().unwrap().record_raster_batch(
                        command,
                        draws.iter().map(|draw| {
                            (
                                self.resource(draw.vertices).buffer().handle,
                                self.resource(draw.indices).buffer().handle,
                                draw.index_count,
                                &draw.parameters,
                            )
                        }),
                        self.resource(*color).image(),
                        self.resource(*depth).image(),
                        *clear_color,
                        *clear_depth,
                    ),
                    PreparedOperation::ClearAttachments {
                        color,
                        depth,
                        clear_color,
                        clear_depth,
                    } => {
                        record_clear_attachments(
                            device.raw(),
                            command,
                            self.resource(*color).image(),
                            self.resource(*depth).image(),
                            *clear_color,
                            *clear_depth,
                        );
                    }
                    PreparedOperation::Composite { dst, .. } => {
                        self.programs.as_ref().unwrap().record_composite(
                            command,
                            pass.descriptor.unwrap(),
                            self.resource(*dst).image(),
                        )
                    }
                }
                #[cfg(feature = "vulkan-diagnostics")]
                device.debug_utils().cmd_end_debug_utils_label(command);
            }
        }
        if !self.readbacks.is_empty() {
            // SAFETY: Only retained copy-written readback ranges are host-read.
            // CPU access additionally requires successful Fence wait/invalidate.
            unsafe {
                super::super::super::transfer::record_host_read(device, command);
            }
        }
        if let Some(lease) = self
            .profile
            .as_ref()
            .and_then(|profile| profile.lease.as_ref())
        {
            // SAFETY: The matching start precedes every Graph command and this
            // end timestamp follows all rendering/readback scopes.
            unsafe { lease.end(command) };
        }
    }

    fn submitted(&mut self) {
        // No allocation, logging or application callbacks at this commit point.
        for (index, physical) in self.physical.iter().enumerate() {
            if let Some(physical) = physical {
                physical.lock().scheduled = self.final_states[index].take().unwrap();
            }
        }
    }

    unsafe fn establish_contents(&mut self) {
        if let Some(profile) = &mut self.profile {
            profile.begin_retire();
        }
        for physical in self.physical.iter().flatten() {
            let mut state = physical.lock();
            // Waiting A after B must never overwrite B's accepted state, or
            // a later completion already observed by another submission.
            state.completed_serial = state.completed_serial.max(self.serial);
        }
        for (_, range) in &mut self.readbacks {
            // SAFETY: Caller obtained actual completion of this whole payload.
            unsafe {
                range.as_mut().unwrap().establish_contents();
            }
        }
        self.passes.clear();
        self.uploads.clear();
        self.physical.clear();
        self.bindings.take();
        self.programs.take();
    }

    fn retired(&mut self) {
        if let Some(profile) = &mut self.profile {
            profile.end_retire();
        }
    }
}

/// # Safety
/// The caller records outside rendering and has transitioned both retained
/// images into their attachment layouts with the planned dependencies.
unsafe fn record_clear_attachments(
    device: &ash::Device,
    command: vk::CommandBuffer,
    color: &DeviceImage,
    depth: &DeviceImage,
    clear_color: [f32; 4],
    clear_depth: f32,
) {
    let colors = [vk::RenderingAttachmentInfo::default()
        .image_view(color.view)
        .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .clear_value(vk::ClearValue {
            color: vk::ClearColorValue {
                float32: clear_color,
            },
        })];
    let depth = vk::RenderingAttachmentInfo::default()
        .image_view(depth.view)
        .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .clear_value(vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: clear_depth,
                stencil: 0,
            },
        });
    let info = vk::RenderingInfo::default()
        .render_area(vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: color.extent,
        })
        .layer_count(1)
        .color_attachments(&colors)
        .depth_attachment(&depth);
    // SAFETY: The caller establishes the documented layout, dependency,
    // lifetime and recording conditions; no pipeline is needed for load clears.
    unsafe {
        device.cmd_begin_rendering(command, &info);
        device.cmd_end_rendering(command);
    }
}

fn layers(image: &DeviceImage) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default()
        .aspect_mask(image.aspect)
        .layer_count(1)
}
fn image_copy(image: &DeviceImage, offset: u64) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .buffer_offset(offset)
        .image_subresource(layers(image))
        .image_extent(vk::Extent3D {
            width: image.extent.width,
            height: image.extent.height,
            depth: 1,
        })
}
fn load(value: LoadAction) -> vk::AttachmentLoadOp {
    match value {
        LoadAction::Clear => vk::AttachmentLoadOp::CLEAR,
        LoadAction::Load => vk::AttachmentLoadOp::LOAD,
        LoadAction::Discard => vk::AttachmentLoadOp::DONT_CARE,
    }
}
fn store(value: StoreOp) -> vk::AttachmentStoreOp {
    match value {
        StoreOp::Store => vk::AttachmentStoreOp::STORE,
        StoreOp::Discard => vk::AttachmentStoreOp::DONT_CARE,
    }
}

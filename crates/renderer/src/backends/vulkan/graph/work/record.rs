//! Native command recording and completion for an already prepared Graph.

use std::ffi::CStr;
use std::sync::Arc;

use ash::vk;

use super::super::super::memory::MemoryOwner;
use super::super::super::resources::DeviceImage;
use super::super::super::transfer::staging::StagingRange;
use super::super::super::transfer::{TransferError, TransferPayload};
use super::{ComputeTransform, GraphWork, PreparedOperation};

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
        self.data
            .as_ref()
            .into_iter()
            .flat_map(|data| data.uploads.iter())
            .chain(
                self.result
                    .readbacks
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
                    &vk::DebugUtilsLabelEXT::default().label_name(
                        // SAFETY: Preparation appended one NUL after checking
                        // the copied label has no interior NUL; backing is held.
                        CStr::from_bytes_with_nul_unchecked(&self.labels[pass.name.clone()]),
                    ),
                );
                if !pass.buffers.is_empty() || !pass.images.is_empty() {
                    device.raw().cmd_pipeline_barrier2(
                        command,
                        &vk::DependencyInfo::default()
                            .buffer_memory_barriers(&self.buffers[pass.buffers.clone()])
                            .image_memory_barriers(&self.images[pass.images.clone()]),
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
                        let range = self.result.readbacks[pass.staging.unwrap()]
                            .1
                            .as_ref()
                            .unwrap();
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
                            // Packed upload data contains every mip in order.
                            // Record each region directly without frame allocation.
                            &[image_copy(image, range.offset(), 0)],
                        );
                        let mut offset = range.offset() + mip_bytes(image, 0);
                        for mip in 1..image.mip_levels {
                            device.raw().cmd_copy_buffer_to_image(
                                command,
                                range.transfer_handle(),
                                image.image,
                                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                                &[image_copy(image, offset, mip)],
                            );
                            offset += mip_bytes(image, mip);
                        }
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
                        let range = self.result.readbacks[pass.staging.unwrap()]
                            .1
                            .as_ref()
                            .unwrap();
                        let image = self.resource(*src).image();
                        device.raw().cmd_copy_image_to_buffer(
                            command,
                            image.image,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            range.transfer_handle(),
                            &[image_copy(image, range.offset(), 0)],
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
                        program,
                        draws,
                        colors,
                        depth,
                    } => {
                        let plan = self.plan.as_ref().unwrap();
                        let programs = self.programs.as_ref().unwrap();
                        let native_program = programs
                            .raster_program(*program, depth.is_some())
                            .expect("prepared Raster Program");
                        let color = plan.raster_attachments(colors).first().map(|attachment| {
                            (
                                self.resource(attachment.view.image().index).image(),
                                attachment,
                            )
                        });
                        let depth = depth.map(|index| {
                            let attachment = plan.raster_attachment(index);
                            (
                                self.resource(attachment.view.image().index).image(),
                                attachment,
                            )
                        });
                        programs.record_rasters(
                            command,
                            native_program,
                            plan.raster_draws(draws)
                                .iter()
                                .zip(&self.raster_descriptors[pass.raster_descriptors.clone()])
                                .map(|(draw, &sets)| {
                                    (
                                        self.resource(draw.vertices).buffer().handle,
                                        self.resource(draw.indices).buffer().handle,
                                        draw.index_count,
                                        &draw.parameters,
                                        sets,
                                    )
                                }),
                            color,
                            depth,
                        );
                    }
                    PreparedOperation::ClearAttachments { colors, depth } => {
                        let plan = self.plan.as_ref().unwrap();
                        let color = plan.raster_attachments(colors).first().map(|attachment| {
                            (
                                self.resource(attachment.view.image().index).image(),
                                attachment,
                            )
                        });
                        let depth = depth.map(|index| {
                            let attachment = plan.raster_attachment(index);
                            (
                                self.resource(attachment.view.image().index).image(),
                                attachment,
                            )
                        });
                        record_clear_attachments(device.raw(), command, color, depth);
                    }
                    PreparedOperation::Dispatch { workgroups } => {
                        self.compute.as_ref().unwrap().record(
                            command,
                            pass.compute.unwrap(),
                            *workgroups,
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
        if !self.result.readbacks.is_empty() {
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
        let data = &mut **self.data.as_mut().unwrap();
        for index in 0..data.active_resources {
            let Some(physical) = &data.physical[index] else {
                continue;
            };
            std::mem::swap(
                &mut physical.lock().scheduled,
                data.final_states[index].as_mut().unwrap(),
            );
        }
    }

    unsafe fn establish_contents(&mut self) {
        if let Some(profile) = &mut self.profile {
            profile.begin_retire();
        }
        for physical in self.physical[..self.active_resources].iter().flatten() {
            let mut state = physical.lock();
            // Waiting A after B must never overwrite B's accepted state, or
            // a later completion already observed by another submission.
            state.completed_serial = state.completed_serial.max(self.serial);
        }
        for (_, range) in &mut self.result.readbacks {
            // SAFETY: Caller obtained actual completion of this whole payload.
            unsafe {
                range.as_mut().unwrap().establish_contents();
            }
        }
        self.bindings.take();
        self.compute.take();
        self.programs.take();
        self.plan.take();
        self.data.take();
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
    color: Option<(&DeviceImage, &crate::graph::AttachmentOperand)>,
    depth: Option<(&DeviceImage, &crate::graph::AttachmentOperand)>,
) {
    let extent = color
        .map(|(image, _)| image.extent)
        .or_else(|| depth.map(|(image, _)| image.extent))
        .unwrap();
    let colors = [color.map_or_else(
        vk::RenderingAttachmentInfo::default,
        |(image, attachment)| super::super::super::program::attachment_info(image, attachment),
    )];
    let depth_attachment = depth.map(|(image, attachment)| {
        super::super::super::program::attachment_info(image, attachment)
    });
    let mut info = vk::RenderingInfo::default()
        .render_area(vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        })
        .layer_count(1)
        .color_attachments(if color.is_some() { &colors } else { &[] });
    if let Some(attachment) = &depth_attachment {
        info = info.depth_attachment(attachment);
    }
    // SAFETY: Caller's retained images are transitioned and loads validated;
    // no shader commands/barriers execute inside this attachment-only scope.
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
fn mip_bytes(image: &DeviceImage, mip: u32) -> u64 {
    let texel_bytes = if image.format == vk::Format::R16G16B16A16_SFLOAT {
        8
    } else {
        4
    };
    u64::from((image.extent.width >> mip).max(1))
        * u64::from((image.extent.height >> mip).max(1))
        * texel_bytes
}
fn image_copy(image: &DeviceImage, offset: u64, mip: u32) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .buffer_offset(offset)
        .image_subresource(layers(image).mip_level(mip))
        .image_extent(vk::Extent3D {
            width: (image.extent.width >> mip).max(1),
            height: (image.extent.height >> mip).max(1),
            depth: 1,
        })
}

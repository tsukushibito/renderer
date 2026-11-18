//! Native command recording and completion for an already prepared Graph.

use std::ffi::CStr;
use std::ops::Range;
use std::sync::Arc;

use ash::vk;

use super::super::super::memory::MemoryOwner;
use super::super::super::program::{RenderingAttachments, attachment_info};
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
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.vulkan.graph.record"),
                0,
            )
        });
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
            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!("renderer.vulkan.graph.record_pass"),
                    0,
                )
            });
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
                    #[cfg(feature = "tracy")]
                    let _span = tracy_client::Client::running().map(|client| {
                        client.span(
                            tracy_client::span_location!("renderer.vulkan.graph.record_barriers"),
                            0,
                        )
                    });
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
                        batches,
                        colors,
                        depth,
                        sample_count,
                    } => {
                        #[cfg(feature = "tracy")]
                        let _span = tracy_client::Client::running().map(|client| {
                            client.span(
                                tracy_client::span_location!("renderer.vulkan.raster.record"),
                                0,
                            )
                        });
                        let plan = self.plan.as_ref().unwrap();
                        let programs = self.programs.as_ref().unwrap();
                        let attachments = rendering_attachments(self, colors, *depth);
                        programs.begin_raster(command, &attachments);
                        let mut descriptor_start = pass.raster_descriptors.start;
                        for (batch, &slot) in plan
                            .raster_batches(batches)
                            .iter()
                            .zip(&self.raster_programs[pass.raster_programs.clone()])
                        {
                            let draw_operands = plan.raster_draws(&batch.draws);
                            let descriptor_end = descriptor_start + draw_operands.len();
                            let descriptor_sets =
                                &self.raster_descriptors[descriptor_start..descriptor_end];
                            descriptor_start = descriptor_end;
                            programs.record_raster_batch(
                                command,
                                programs.raster_program(slot),
                                *sample_count,
                                draw_operands
                                    .iter()
                                    .zip(descriptor_sets)
                                    .map(|(draw, &sets)| {
                                        (
                                            self.resource(draw.vertices).buffer().handle,
                                            self.resource(draw.indices).buffer().handle,
                                            draw.index_count,
                                            &draw.parameters,
                                            sets,
                                        )
                                    }),
                            );
                        }
                        device.raw().cmd_end_rendering(command);
                    }
                    PreparedOperation::ClearAttachments { colors, depth } => {
                        let attachments = rendering_attachments(self, colors, *depth);
                        device
                            .raw()
                            .cmd_begin_rendering(command, &attachments.info());
                        device.raw().cmd_end_rendering(command);
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

/// Every attachment and its resolve target are retained by this work and
/// transitioned by the owning pass before either begins rendering.
fn rendering_attachments(
    work: &GraphWork,
    colors: &Range<usize>,
    depth: Option<usize>,
) -> RenderingAttachments<'static> {
    let plan = work.plan.as_ref().unwrap();
    let prepare_attachment = |attachment: &crate::graph::AttachmentOperand| {
        let image = work.resource(attachment.view.image().index).image();
        let resolve = attachment
            .resolve
            .map(|view| work.resource(view.image().index).image());
        (image.extent, attachment_info(image, attachment, resolve))
    };
    let attachment_operands = plan.raster_attachments(colors);
    let mut native_colors =
        [vk::RenderingAttachmentInfo::default(); crate::program::MAX_RASTER_COLOR_ATTACHMENTS];
    let mut extent = None;
    for (target, operand) in native_colors[..attachment_operands.len()]
        .iter_mut()
        .zip(attachment_operands)
    {
        let (current_extent, info) = prepare_attachment(operand);
        extent = Some(current_extent);
        *target = info;
    }
    let depth = depth.map(|index| prepare_attachment(plan.raster_attachment(index)));
    RenderingAttachments {
        extent: extent
            .or(depth.map(|(extent, _)| extent))
            .expect("Graph rendering has attachments"),
        colors: native_colors,
        color_count: attachment_operands.len(),
        depth: depth.map(|(_, info)| info),
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

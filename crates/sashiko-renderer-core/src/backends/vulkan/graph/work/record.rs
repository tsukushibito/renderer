//! Native command recording and completion for an already prepared Graph.

use std::ffi::CStr;
use std::ops::Range;
use std::sync::Arc;

use ash::vk;

use super::super::super::memory::MemoryOwner;
use super::super::super::program::{
    NativeIndexedDraw, NativeRasterDraw, NativeRasterGeometry, RenderingAttachments,
    attachment_info,
};
use super::super::super::resources::DeviceImage;
use super::super::super::transfer::staging::StagingRange;
use super::super::super::transfer::{TransferError, TransferPayload};
use super::{GraphWork, PreparedOperation};

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
                    PreparedOperation::UploadImage { dst, region } => {
                        let range = &self.uploads[pass.staging.unwrap()];
                        let image = self.resource(*dst).image();
                        if let Some(region) = region {
                            device.raw().cmd_copy_buffer_to_image(
                                command,
                                range.transfer_handle(),
                                image.image,
                                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                                &[vk::BufferImageCopy::default()
                                    .buffer_offset(range.offset())
                                    .image_subresource(layers(image).mip_level(region.mip_level))
                                    .image_offset(copy_origin(region.origin))
                                    .image_extent(copy_extent(region.extent))],
                            );
                        } else {
                            let mut offset = range.offset();
                            for mip in 0..image.mip_levels {
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
                    }
                    PreparedOperation::CopyImage {
                        src,
                        dst,
                        source,
                        destination,
                    } => {
                        let src = self.resource(*src).image();
                        let dst = self.resource(*dst).image();
                        device.raw().cmd_copy_image(
                            command,
                            src.image,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            dst.image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[vk::ImageCopy::default()
                                .src_subresource(layers(src).mip_level(source.mip_level))
                                .src_offset(copy_origin(source.origin))
                                .dst_subresource(layers(dst).mip_level(destination.mip_level))
                                .dst_offset(copy_origin(destination.origin))
                                .extent(copy_extent(source.extent))],
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
                        if let Some(secondary) = &self.secondary
                            && !pass.raster_programs.is_empty()
                        {
                            let info = attachments
                                .info()
                                .flags(vk::RenderingFlags::CONTENTS_SECONDARY_COMMAND_BUFFERS);
                            device.raw().cmd_begin_rendering(command, &info);
                            device.raw().cmd_execute_commands(
                                command,
                                &secondary.handles[pass.raster_programs.clone()],
                            );
                        } else {
                            programs.begin_raster(command, &attachments);
                            let mut regions = None;
                            let mut descriptor_start = pass.raster_descriptors.start;
                            for (batch, &slot) in plan
                                .raster_batches(batches)
                                .iter()
                                .zip(&self.raster_programs[pass.raster_programs.clone()])
                            {
                                if regions != Some(batch.regions) {
                                    programs.set_raster_regions(
                                        command,
                                        batch.regions,
                                        attachments.extent,
                                    );
                                    regions = Some(batch.regions);
                                }
                                let draw_operands = plan.raster_draws(&batch.draws);
                                let descriptor_end = descriptor_start + draw_operands.len();
                                let descriptor_sets =
                                    &self.raster_descriptors[descriptor_start..descriptor_end];
                                descriptor_start = descriptor_end;
                                record_batch(
                                    command,
                                    programs,
                                    slot,
                                    *sample_count,
                                    plan,
                                    batch,
                                    descriptor_sets,
                                    &self.physical,
                                );
                            }
                        }
                        device.raw().cmd_end_rendering(command);
                    }
                    PreparedOperation::ClearAttachments { colors, depth } => {
                        let mut attachments = rendering_attachments(self, colors, *depth);
                        let rect = depth.and_then(|index| {
                            self.plan
                                .as_ref()
                                .unwrap()
                                .raster_attachment(index)
                                .depth_clear_rect
                        });
                        if rect.is_some() {
                            let depth = attachments.depth.as_mut().expect("depth clear target");
                            depth.load_op = vk::AttachmentLoadOp::CLEAR;
                            depth.clear_value = vk::ClearValue {
                                depth_stencil: vk::ClearDepthStencilValue {
                                    depth: 1.0,
                                    stencil: 0,
                                },
                            };
                        }
                        let mut info = attachments.info();
                        if let Some(rect) = rect {
                            info.render_area = vk::Rect2D {
                                offset: vk::Offset2D {
                                    x: rect.x as i32,
                                    y: rect.y as i32,
                                },
                                extent: vk::Extent2D {
                                    width: rect.width,
                                    height: rect.height,
                                },
                            };
                        }
                        device.raw().cmd_begin_rendering(command, &info);
                        device.raw().cmd_end_rendering(command);
                    }
                    PreparedOperation::Dispatch { workgroups } => {
                        self.compute.as_ref().unwrap().record(
                            command,
                            pass.compute.unwrap(),
                            *workgroups,
                        );
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
        self.secondary.take();
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
fn copy_origin([x, y]: [u32; 2]) -> vk::Offset3D {
    vk::Offset3D {
        x: x as i32,
        y: y as i32,
        z: 0,
    }
}
fn copy_extent([width, height]: [u32; 2]) -> vk::Extent3D {
    vk::Extent3D {
        width,
        height,
        depth: 1,
    }
}
fn layers(image: &DeviceImage) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default()
        .aspect_mask(image.aspect)
        .layer_count(1)
}
fn mip_bytes(image: &DeviceImage, mip: u32) -> u64 {
    image
        .logical_format
        .packed_byte_len(
            (image.extent.width >> mip).max(1),
            (image.extent.height >> mip).max(1),
        )
        .expect("validated image byte length")
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

#[allow(clippy::too_many_arguments)]
unsafe fn record_batch(
    command: vk::CommandBuffer,
    programs: &super::ShaderPrograms,
    slot: super::RasterProgramSlot,
    sample_count: crate::graph::SampleCount,
    plan: &crate::graph::CompiledGraph,
    batch: &crate::graph::RasterBatchOperands,
    descriptor_sets: &[[vk::DescriptorSet; crate::program::MAX_RASTER_BIND_GROUPS]],
    physical: &[Option<super::super::PhysicalLease>],
) {
    let draw_operands = plan.raster_draws(&batch.draws);
    let indirect = batch.indirect.map(|args| NativeIndexedDraw::Indirect {
        buffer: physical[args.buffer].as_ref().unwrap().buffer().handle,
        command_count: args.command_count,
    });
    let draws = draw_operands
        .iter()
        .zip(descriptor_sets)
        .zip(plan.raster_constants(batch))
        .map(|((draw, &sets), parameters)| {
            let geometry = match &draw.indexed {
                None => NativeRasterGeometry::Fullscreen,
                Some(indexed) => {
                    let vertices = physical[indexed.vertices].as_ref().unwrap().buffer();
                    let indices = physical[indexed.indices].as_ref().unwrap().buffer();
                    let command = indirect.unwrap_or(NativeIndexedDraw::Direct {
                        first_index: indexed.first_index,
                        index_count: indexed.index_count,
                        instance_count: indexed.instance_count,
                    });
                    NativeRasterGeometry::Indexed {
                        vertices: vertices.handle,
                        secondary_vertices: indexed
                            .secondary_vertices
                            .map(|id| physical[id].as_ref().unwrap().buffer().handle),
                        indices: indices.handle,
                        command,
                    }
                }
            };
            NativeRasterDraw {
                geometry,
                parameters,
                sets,
            }
        });
    // SAFETY: Caller owns the recording and supplies the prepared batch and live operands.
    unsafe {
        programs.record_raster_batch(command, programs.raster_program(slot), sample_count, draws)
    };
}

impl GraphWork {
    pub(in crate::backends::vulkan::graph) fn record_secondaries(
        &mut self,
        recorder: &mut super::super::secondary::Recorder,
    ) -> Result<(), super::RuntimeError> {
        let count = self.raster_programs.len();
        if count == 0 || count > recorder.capacity {
            return Ok(());
        }
        let plan = self.plan.as_ref().unwrap();
        if recorder.team.automatic {
            let sizes = self
                .passes
                .iter()
                .filter_map(|pass| match &pass.operation {
                    PreparedOperation::Raster { batches, .. } => Some(plan.raster_batches(batches)),
                    _ => None,
                })
                .flatten()
                .filter(|batch| batch.indirect.is_none())
                .map(|batch| batch.draws.len());
            if !recorder.team.large_enough(8, sizes) {
                return Ok(());
            }
        }
        let mut commands = recorder.storage.acquire()?;
        let programs = self.programs.as_ref().unwrap();
        let data = &**self;
        for (pass_index, pass) in data.passes.iter().enumerate() {
            if let PreparedOperation::Raster { batches, .. } = &pass.operation {
                let mut descriptors = pass.raster_descriptors.start;
                for (lane, batch) in commands.lanes[pass.raster_programs.clone()]
                    .iter_mut()
                    .zip(batches.clone())
                {
                    lane.pass = pass_index;
                    lane.batch = batch;
                    lane.descriptors = descriptors;
                    descriptors += plan.raster_batches(&(batch..batch + 1))[0].draws.len();
                }
            }
        }
        let lanes = &mut commands.lanes[..count];
        let passes = &data.passes;
        let physical = &data.physical;
        let descriptors = &data.raster_descriptors;
        let slots = &data.raster_programs;
        recorder.team.jobs.with(|team| {
            team.scope(|scope| {
                scope.for_each_chunk_mut(lanes, std::num::NonZeroUsize::MIN, |index, lanes| {
                    let lane = &mut lanes[0];
                    let PreparedOperation::Raster {
                        colors,
                        depth,
                        sample_count,
                        ..
                    } = &passes[lane.pass].operation
                    else {
                        unreachable!()
                    };
                    let mut formats =
                        [vk::Format::UNDEFINED; crate::program::MAX_RASTER_COLOR_ATTACHMENTS];
                    let mut extent = vk::Extent2D::default();
                    for (format, target) in formats.iter_mut().zip(plan.raster_attachments(colors))
                    {
                        let image = physical[target.view.image().index]
                            .as_ref()
                            .unwrap()
                            .image();
                        *format = image.format;
                        extent = image.extent;
                    }
                    let depth_format = depth.map_or(vk::Format::UNDEFINED, |target| {
                        let image = physical[plan.raster_attachment(target).view.image().index]
                            .as_ref()
                            .unwrap()
                            .image();
                        extent = image.extent;
                        image.format
                    });
                    let mut rendering = vk::CommandBufferInheritanceRenderingInfo::default()
                        .color_attachment_formats(&formats[..colors.len()])
                        .depth_attachment_format(depth_format)
                        .rasterization_samples(
                            crate::backends::vulkan::resources::sample_count_flags(*sample_count),
                        );
                    let inheritance =
                        vk::CommandBufferInheritanceInfo::default().push_next(&mut rendering);
                    let batch = &plan.raster_batches(&(lane.batch..lane.batch + 1))[0];
                    let sets = &descriptors[lane.descriptors..lane.descriptors + batch.draws.len()];
                    lane.error = lane
                        .record(&inheritance, |command| {
                            // SAFETY: This batch exclusively owns its secondary, and the Graph
                            // retains exactly the operands validated for this rendering scope.
                            unsafe {
                                programs.set_raster_regions(command, batch.regions, extent);
                                record_batch(
                                    command,
                                    programs,
                                    slots[index],
                                    *sample_count,
                                    plan,
                                    batch,
                                    sets,
                                    physical,
                                );
                            }
                        })
                        .err();
                })
            })
        });
        for lane in lanes {
            if let Some(error) = lane.error.take() {
                return Err(error);
            }
        }
        self.secondary = Some(commands);
        Ok(())
    }
}

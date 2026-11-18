//! Native recording for the current fixed shader ABI.
//!
//! This layer consumes prepared operands. Synchronization and lifetime proof
//! belong to the executor; it performs no creation or descriptor mutation.

use ash::vk;

use super::super::resources::DeviceImage;
use super::ShaderPrograms;
use crate::graph::{RasterConstants, SampleCount};

pub(in crate::backends::vulkan) struct RenderingAttachments<'a> {
    pub extent: vk::Extent2D,
    pub color: Option<vk::RenderingAttachmentInfo<'a>>,
    pub depth: Option<vk::RenderingAttachmentInfo<'a>>,
}

impl RenderingAttachments<'_> {
    pub(in crate::backends::vulkan) fn info(&self) -> vk::RenderingInfo<'_> {
        let mut info = vk::RenderingInfo::default()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: self.extent,
            })
            .layer_count(1)
            .color_attachments(self.color.as_slice());
        if let Some(attachment) = &self.depth {
            info = info.depth_attachment(attachment);
        }
        info
    }
}

impl ShaderPrograms {
    /// # Safety
    /// `command` is recording outside rendering. Bindings/images/programs are
    /// retained through GPU retirement; prior dependencies establish GENERAL
    /// src/dst layouts and shader read/write visibility. `mode` is 0 or 1.
    pub(in crate::backends::vulkan) unsafe fn record_compute(
        &self,
        command: vk::CommandBuffer,
        set: vk::DescriptorSet,
        destination: &DeviceImage,
        mode: u32,
    ) {
        let device = self.owner.device.raw();
        // SAFETY: The executor satisfies the documented retention/layout/ABI
        // contract; validated descriptor set is immutable during command use.
        unsafe {
            device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::COMPUTE,
                self.compute().pipeline,
            );
            device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::COMPUTE,
                self.compute().layout,
                0,
                &[set],
                &[],
            );
            device.cmd_push_constants(
                command,
                self.compute().layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                &mode.to_ne_bytes(),
            );
            device.cmd_dispatch(
                command,
                destination.extent.width.div_ceil(8),
                destination.extent.height.div_ceil(8),
                1,
            );
        }
    }

    /// # Safety
    /// The command is recording outside rendering. All ordered draws, Programs
    /// and attachments survive actual GPU completion; indices/ranges/ABI obey
    /// the reviewed mesh contract. Dependencies establish attachment layouts
    /// and valid contents for LOAD before this scope; no storage side effects
    /// or barriers are admitted within the rendering scope.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::backends::vulkan) unsafe fn record_rasters<'a>(
        &self,
        command: vk::CommandBuffer,
        program: &super::Program,
        sample_count: SampleCount,
        draws: impl IntoIterator<
            Item = (
                vk::Buffer,
                vk::Buffer,
                u32,
                &'a RasterConstants,
                [vk::DescriptorSet; crate::program::MAX_RASTER_BIND_GROUPS],
            ),
        >,
        attachments: &RenderingAttachments<'_>,
    ) {
        let device = self.owner.device.raw();
        let pipeline = program
            .raster_pipeline(sample_count)
            .expect("prepared Raster pipeline for attachment sample count");
        // SAFETY: Caller retains compatible pipeline, descriptors, attachments
        // and buffers through actual retirement; all dependencies precede scope.
        unsafe {
            device.cmd_begin_rendering(command, &attachments.info());
            device.cmd_bind_pipeline(command, vk::PipelineBindPoint::GRAPHICS, pipeline);
            self.viewport(command, attachments.extent);
            {
                #[cfg(feature = "tracy")]
                let _span = tracy_client::Client::running().map(|client| {
                    client.span(
                        tracy_client::span_location!("renderer.vulkan.raster.bind_and_draw_batch"),
                        0,
                    )
                });
                for (vertices, indices, index_count, parameters, sets) in draws {
                    let managed = program
                        .raster_asset
                        .unwrap_or_else(crate::program::ManagedRasterProgram::mesh);
                    let size = managed.parameter_size() as usize;
                    if size != 0 {
                        let mut bytes = [0_u8; 128];
                        for (value, output) in
                            parameters.words().iter().zip(bytes.as_chunks_mut::<4>().0)
                        {
                            output.copy_from_slice(&value.to_ne_bytes());
                        }
                        device.cmd_push_constants(
                            command,
                            program.layout,
                            super::creation::shader_stages(managed.parameter_layout().stages),
                            0,
                            &bytes[..size],
                        );
                    }
                    if program.raster_groups != 0 {
                        device.cmd_bind_descriptor_sets(
                            command,
                            vk::PipelineBindPoint::GRAPHICS,
                            program.layout,
                            0,
                            &sets[..program.raster_groups],
                            &[],
                        );
                    }
                    device.cmd_bind_vertex_buffers(command, 0, &[vertices], &[0]);
                    device.cmd_bind_index_buffer(command, indices, 0, vk::IndexType::UINT32);
                    device.cmd_draw_indexed(command, index_count, 1, 0, 0, 0);
                }
            }
            device.cmd_end_rendering(command);
        }
    }

    /// # Safety
    /// Recording outside rendering; descriptor source is SHADER_READ_ONLY and
    /// destination COLOR_ATTACHMENT_OPTIMAL with required dependencies. Both
    /// images, immutable descriptor set and programs survive retirement; extents
    /// match, source and destination differ. Fullscreen draw initializes all pixels.
    pub(in crate::backends::vulkan) unsafe fn record_composite(
        &self,
        command: vk::CommandBuffer,
        set: vk::DescriptorSet,
        destination: &DeviceImage,
    ) {
        let colors = [vk::RenderingAttachmentInfo::default()
            .image_view(destination.view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::STORE)];
        let info = vk::RenderingInfo::default()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: destination.extent,
            })
            .layer_count(1)
            .color_attachments(&colors);
        let device = self.owner.device.raw();
        // SAFETY: The caller guarantees the documented synchronization and
        // descriptor/resource retention. Fixed triangle covers every pixel.
        unsafe {
            device.cmd_begin_rendering(command, &info);
            device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.composite().pipeline,
            );
            self.viewport(command, destination.extent);
            device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.composite().layout,
                0,
                &[set],
                &[],
            );
            device.cmd_draw(command, 3, 1, 0, 0);
            device.cmd_end_rendering(command);
        }
    }

    unsafe fn viewport(&self, command: vk::CommandBuffer, extent: vk::Extent2D) {
        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: extent.width as f32,
            height: extent.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let scissor = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        };
        // SAFETY: Called only inside our rendering scope with queried dimensions
        // and dynamically enabled viewport/scissor state.
        unsafe {
            self.owner
                .device
                .raw()
                .cmd_set_viewport(command, 0, &[viewport]);
            self.owner
                .device
                .raw()
                .cmd_set_scissor(command, 0, &[scissor]);
        }
    }
}

/// Native interpretation of one owned, validated attachment.
pub(in crate::backends::vulkan) fn attachment_info<'a>(
    image: &DeviceImage,
    attachment: &crate::graph::AttachmentOperand,
    resolve: Option<&DeviceImage>,
) -> vk::RenderingAttachmentInfo<'a> {
    use crate::graph::{LoadAction, StoreOp};
    let depth = image.aspect == vk::ImageAspectFlags::DEPTH;
    let mut info = vk::RenderingAttachmentInfo::default()
        .image_view(image.view)
        .image_layout(if depth {
            vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL
        } else {
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
        })
        .load_op(match attachment.ops.load {
            LoadAction::Load => vk::AttachmentLoadOp::LOAD,
            LoadAction::Clear => vk::AttachmentLoadOp::CLEAR,
            LoadAction::Discard => vk::AttachmentLoadOp::DONT_CARE,
        })
        .store_op(match attachment.ops.store {
            StoreOp::Store => vk::AttachmentStoreOp::STORE,
            StoreOp::Discard => vk::AttachmentStoreOp::DONT_CARE,
        })
        .clear_value(if depth {
            vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: attachment.clear[0],
                    stencil: 0,
                },
            }
        } else {
            vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: attachment.clear,
                },
            }
        });
    if let Some(resolve) = resolve {
        info = info
            .resolve_mode(vk::ResolveModeFlags::AVERAGE)
            .resolve_image_view(resolve.view)
            .resolve_image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    }
    info
}

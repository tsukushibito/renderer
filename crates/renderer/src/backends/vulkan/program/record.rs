//! Native recording for the prepared Raster shader ABI.
//!
//! This layer consumes prepared operands. Synchronization and lifetime proof
//! belong to the executor; it performs no creation or descriptor mutation.

use ash::vk;

use super::super::resources::DeviceImage;
use super::ShaderPrograms;
use crate::graph::SampleCount;

const _: () = assert!(
    std::mem::size_of::<vk::DrawIndexedIndirectCommand>() as u64
        == crate::program::INDIRECT_COMMAND_BYTES
);

#[derive(Clone, Copy)]
pub(in crate::backends::vulkan) enum NativeIndexedDraw {
    Direct {
        first_index: u32,
        index_count: u32,
        instance_count: u32,
    },
    Indirect {
        buffer: vk::Buffer,
        command_count: u32,
    },
}

pub(in crate::backends::vulkan) struct NativeRasterDraw<'a> {
    pub vertices: vk::Buffer,
    pub indices: vk::Buffer,
    pub command: NativeIndexedDraw,
    pub parameters: &'a [u32],
    pub sets: [vk::DescriptorSet; crate::program::MAX_RASTER_BIND_GROUPS],
}

pub(in crate::backends::vulkan) struct RenderingAttachments<'a> {
    pub extent: vk::Extent2D,
    pub colors: [vk::RenderingAttachmentInfo<'a>; crate::program::MAX_RASTER_COLOR_ATTACHMENTS],
    pub color_count: usize,
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
            .color_attachments(&self.colors[..self.color_count]);
        if let Some(attachment) = &self.depth {
            info = info.depth_attachment(attachment);
        }
        info
    }
}

impl ShaderPrograms {
    /// # Safety
    /// The command is recording outside rendering. The caller retains the
    /// attachments through GPU completion and applies their dependencies,
    /// including valid contents for LOAD, before calling this method.
    pub(in crate::backends::vulkan) unsafe fn begin_raster(
        &self,
        command: vk::CommandBuffer,
        attachments: &RenderingAttachments<'_>,
    ) {
        // SAFETY: The caller established the attachment and command state.
        unsafe {
            self.owner
                .device
                .raw()
                .cmd_begin_rendering(command, &attachments.info());
            self.viewport(command, attachments.extent);
        }
    }

    /// # Safety
    /// The command is recording inside a rendering scope compatible with the
    /// prepared Program and sample count. The owner, descriptors and buffers
    /// must survive GPU completion. Indices, ranges and shader inputs must
    /// obey the validated draw contract. Indirect arguments must come from
    /// the typed producer synchronized for indirect reads. The draw count
    /// must fit the Device limit with the features enabled at creation.
    pub(in crate::backends::vulkan) unsafe fn record_raster_batch<'a>(
        &self,
        command: vk::CommandBuffer,
        program: &super::Program,
        sample_count: SampleCount,
        draws: impl IntoIterator<Item = NativeRasterDraw<'a>>,
    ) {
        let device = self.owner.device.raw();
        let pipeline = program
            .raster_pipeline(sample_count)
            .expect("prepared Raster pipeline for attachment sample count");
        let managed = program
            .raster_asset
            .unwrap_or_else(crate::program::ManagedRasterProgram::mesh);
        let size = managed.parameter_size() as usize;
        let stages = super::creation::shader_stages(managed.parameter_layout().stages);
        // SAFETY: Dependencies, interface compatibility and retention were
        // established by the caller before entering the rendering scope.
        unsafe {
            device.cmd_bind_pipeline(command, vk::PipelineBindPoint::GRAPHICS, pipeline);
            {
                #[cfg(feature = "tracy")]
                let _span = tracy_client::Client::running().map(|client| {
                    client.span(
                        tracy_client::span_location!("renderer.vulkan.raster.bind_and_draw_batch"),
                        0,
                    )
                });
                for draw in draws {
                    if size != 0 {
                        let mut bytes = [0_u8; 128];
                        for (value, output) in
                            draw.parameters.iter().zip(bytes.as_chunks_mut::<4>().0)
                        {
                            output.copy_from_slice(&value.to_ne_bytes());
                        }
                        device.cmd_push_constants(
                            command,
                            program.layout,
                            stages,
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
                            &draw.sets[..program.raster_groups],
                            &[],
                        );
                    }
                    device.cmd_bind_vertex_buffers(command, 0, &[draw.vertices], &[0]);
                    device.cmd_bind_index_buffer(command, draw.indices, 0, vk::IndexType::UINT32);
                    match draw.command {
                        NativeIndexedDraw::Direct {
                            first_index,
                            index_count,
                            instance_count,
                        } => device.cmd_draw_indexed(
                            command,
                            index_count,
                            instance_count,
                            first_index,
                            0,
                            0,
                        ),
                        NativeIndexedDraw::Indirect {
                            buffer,
                            command_count,
                        } => device.cmd_draw_indexed_indirect(
                            command,
                            buffer,
                            0,
                            command_count,
                            crate::program::INDIRECT_COMMAND_BYTES as u32,
                        ),
                    }
                }
            }
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

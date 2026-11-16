//! Native recording for the current fixed shader ABI.
//!
//! This layer consumes prepared operands. Synchronization and lifetime proof
//! belong to the executor; it performs no creation or descriptor mutation.

use ash::vk;

use super::super::resources::DeviceImage;
use super::ShaderPrograms;
use crate::graph::RasterParameters;

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
        draws: impl IntoIterator<
            Item = (
                vk::Buffer,
                vk::Buffer,
                u32,
                &'a RasterParameters,
                [vk::DescriptorSet; 2],
            ),
        >,
        color: Option<(&DeviceImage, &crate::graph::AttachmentOperand)>,
        depth: Option<(&DeviceImage, &crate::graph::AttachmentOperand)>,
    ) {
        let extent = color
            .map(|(image, _)| image.extent)
            .or_else(|| depth.map(|(image, _)| image.extent))
            .unwrap();
        let colors = [color.map_or_else(
            vk::RenderingAttachmentInfo::default,
            |(image, attachment)| attachment_info(image, attachment),
        )];
        let depth_attachment = depth.map(|(image, attachment)| attachment_info(image, attachment));
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
        let device = self.owner.device.raw();
        // SAFETY: Caller retains compatible pipeline, descriptors, attachments
        // and buffers through actual retirement; all dependencies precede scope.
        unsafe {
            device.cmd_begin_rendering(command, &info);
            device.cmd_bind_pipeline(command, vk::PipelineBindPoint::GRAPHICS, program.pipeline);
            self.viewport(command, extent);
            for (vertices, indices, index_count, parameters, sets) in draws {
                let mut bytes = [0_u8; 80];
                for (value, output) in parameters
                    .transform()
                    .iter()
                    .flatten()
                    .chain(parameters.tint())
                    .zip(bytes.as_chunks_mut::<4>().0)
                {
                    output.copy_from_slice(&value.to_ne_bytes());
                }
                device.cmd_push_constants(
                    command,
                    program.layout,
                    vk::ShaderStageFlags::VERTEX,
                    0,
                    &bytes,
                );
                if sets[0] != vk::DescriptorSet::null() {
                    device.cmd_bind_descriptor_sets(
                        command,
                        vk::PipelineBindPoint::GRAPHICS,
                        program.layout,
                        0,
                        &sets,
                        &[],
                    );
                }
                device.cmd_bind_vertex_buffers(command, 0, &[vertices], &[0]);
                device.cmd_bind_index_buffer(command, indices, 0, vk::IndexType::UINT32);
                device.cmd_draw_indexed(command, index_count, 1, 0, 0, 0);
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
) -> vk::RenderingAttachmentInfo<'a> {
    use crate::graph::{LoadAction, StoreOp};
    let depth = image.aspect == vk::ImageAspectFlags::DEPTH;
    vk::RenderingAttachmentInfo::default()
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
        })
}

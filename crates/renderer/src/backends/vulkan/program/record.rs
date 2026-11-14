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
    /// Command records outside rendering; all supplied resources and programs
    /// survive retirement. Vertex/index ranges obey the fixed ABI and validated
    /// indices. Color/depth are distinct, equal extents and in attachment layouts
    /// after dependencies, with valid contents for LOAD. Attachments are whole.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::backends::vulkan) unsafe fn record_raster(
        &self,
        command: vk::CommandBuffer,
        vertices: vk::Buffer,
        indices: vk::Buffer,
        index_count: u32,
        color: &DeviceImage,
        depth: &DeviceImage,
        color_load: vk::AttachmentLoadOp,
        color_store: vk::AttachmentStoreOp,
        clear_color: [f32; 4],
        depth_load: vk::AttachmentLoadOp,
        depth_store: vk::AttachmentStoreOp,
        clear_depth: f32,
        parameters: &RasterParameters,
    ) {
        // SAFETY: The caller supplies a validated draw and retained attachments.
        unsafe {
            self.record_rasters(
                command,
                std::iter::once((vertices, indices, index_count, parameters)),
                color,
                depth,
                color_load,
                color_store,
                clear_color,
                depth_load,
                depth_store,
                clear_depth,
            );
        }
    }

    /// # Safety
    /// All fixed-pipeline draws are validated and retained through retirement.
    /// External dependencies precede this whole scope. No shader storage writes
    /// occur; draw order preserves primitive-order attachment operations.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::backends::vulkan) unsafe fn record_raster_batch<'a>(
        &self,
        command: vk::CommandBuffer,
        draws: impl IntoIterator<Item = (vk::Buffer, vk::Buffer, u32, &'a RasterParameters)>,
        color: &DeviceImage,
        depth: &DeviceImage,
        clear_color: [f32; 4],
        clear_depth: f32,
    ) {
        // SAFETY: The graph batch declares the full clear and every buffer read;
        // the caller validates indices and establishes all attachment layouts.
        unsafe {
            self.record_rasters(
                command,
                draws,
                color,
                depth,
                vk::AttachmentLoadOp::CLEAR,
                vk::AttachmentStoreOp::STORE,
                clear_color,
                vk::AttachmentLoadOp::CLEAR,
                vk::AttachmentStoreOp::STORE,
                clear_depth,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn record_rasters<'a>(
        &self,
        command: vk::CommandBuffer,
        draws: impl IntoIterator<Item = (vk::Buffer, vk::Buffer, u32, &'a RasterParameters)>,
        color: &DeviceImage,
        depth: &DeviceImage,
        color_load: vk::AttachmentLoadOp,
        color_store: vk::AttachmentStoreOp,
        clear_color: [f32; 4],
        depth_load: vk::AttachmentLoadOp,
        depth_store: vk::AttachmentStoreOp,
        clear_depth: f32,
    ) {
        let color_attachments = [vk::RenderingAttachmentInfo::default()
            .image_view(color.view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(color_load)
            .store_op(color_store)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: clear_color,
                },
            })];
        let depth_attachment = vk::RenderingAttachmentInfo::default()
            .image_view(depth.view)
            .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
            .load_op(depth_load)
            .store_op(depth_store)
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
            .color_attachments(&color_attachments)
            .depth_attachment(&depth_attachment);
        let device = self.owner.device.raw();
        // SAFETY: Caller establishes outside-scope dependencies and valid
        // resource/range/binding/layout state. No barrier occurs in this scope.
        unsafe {
            device.cmd_begin_rendering(command, &info);
            device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.raster().pipeline,
            );
            self.viewport(command, color.extent);
            for (vertices, indices, index_count, parameters) in draws {
                let mut bytes = [0_u8; 80];
                for (value, output) in parameters
                    .transform
                    .iter()
                    .flatten()
                    .chain(&parameters.tint)
                    .zip(bytes.as_chunks_mut::<4>().0)
                {
                    output.copy_from_slice(&value.to_ne_bytes());
                }
                device.cmd_push_constants(
                    command,
                    self.raster().layout,
                    vk::ShaderStageFlags::VERTEX,
                    0,
                    &bytes,
                );
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

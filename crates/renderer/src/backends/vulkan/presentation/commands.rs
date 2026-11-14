//! Native fullscreen presentation commands; Graph leases remain in the adapter.

use super::*;

pub(in crate::backends::vulkan) struct SourceScope {
    pub(in crate::backends::vulkan) stage: vk::PipelineStageFlags2,
    pub(in crate::backends::vulkan) access: vk::AccessFlags2,
    pub(in crate::backends::vulkan) layout: vk::ImageLayout,
}

pub(in crate::backends::vulkan) struct PresentCommands {
    owner: Arc<MemoryOwner>,
    chain: Arc<Swapchain>,
    slot: Arc<Slot>,
    image: u32,
}
impl PresentCommands {
    pub(in crate::backends::vulkan) fn new(
        owner: Arc<MemoryOwner>,
        chain: Arc<Swapchain>,
        slot: Arc<Slot>,
        image: u32,
    ) -> Self {
        Self {
            owner,
            chain,
            slot,
            image,
        }
    }
    pub(in crate::backends::vulkan) fn owner(&self) -> &Arc<MemoryOwner> {
        &self.owner
    }
    pub(in crate::backends::vulkan) fn submitted(&self) {
        self.slot.submitted();
    }
    /// Record the native fullscreen draw using source state supplied by the
    /// canonical Graph adapter. This type never owns or publishes Graph state.
    ///
    /// # Safety
    /// `command` must be recording on this owner's queue family. `source` must
    /// be a live, initialized, same-Device sampled image matching the retained
    /// slot binding and target extent. `previous` must describe its current
    /// canonical same-queue scope/layout. This image index must remain acquired
    /// from the retained chain, and the caller must wait its acquire semaphore
    /// at COLOR_ATTACHMENT_OUTPUT. The caller retains the source allocation,
    /// serializes native queue access, and retains this command owner through
    /// cancellation or actual GPU completion.
    pub(in crate::backends::vulkan) unsafe fn record(
        &self,
        command: vk::CommandBuffer,
        source: vk::Image,
        previous: SourceScope,
    ) {
        let barriers = [
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(previous.stage)
                .src_access_mask(previous.access)
                .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                .old_layout(previous.layout)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(source)
                .subresource_range(color_range()),
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(self.chain.images[self.image as usize])
                .subresource_range(color_range()),
        ];
        let extent = self.chain.extent;
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        };
        let attachments = [vk::RenderingAttachmentInfo::default()
            .image_view(self.chain.views[self.image as usize])
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::STORE)];
        let rendering = vk::RenderingInfo::default()
            .render_area(area)
            .layer_count(1)
            .color_attachments(&attachments);
        let final_barriers = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::NONE)
            .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.chain.images[self.image as usize])
            .subresource_range(color_range())];
        // SAFETY: Payload retains the source, views, descriptors and program.
        // Source state came from the serialized canonical registry; acquire is
        // waited at COLOR_ATTACHMENT_OUTPUT before the full target overwrite.
        // Linear sampled values are encoded by the actual SRGB attachment.
        unsafe {
            let device = self.owner.device.raw();
            device.cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().image_memory_barriers(&barriers),
            );
            device.cmd_begin_rendering(command, &rendering);
            device.cmd_set_viewport(
                command,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_set_scissor(command, 0, &[area]);
            device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.slot.binding.program.pipeline,
            );
            device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.slot.binding.program.layout,
                0,
                &[self.slot.binding.set],
                &[],
            );
            device.cmd_draw(command, 3, 1, 0, 0);
            device.cmd_end_rendering(command);
            device.cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().image_memory_barriers(&final_barriers),
            );
        }
    }
}

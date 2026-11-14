//! Immutable fullscreen presentation program and one reusable descriptor set.

use std::sync::Arc;

use ash::vk;

use super::{MemoryOwner, RuntimeError, native};

pub(in crate::backends::vulkan) struct Program {
    owner: Arc<MemoryOwner>,
    pub(in crate::backends::vulkan) format: vk::Format,
    pub(super) set_layout: vk::DescriptorSetLayout,
    pub(super) layout: vk::PipelineLayout,
    pub(super) pipeline: vk::Pipeline,
    pub(super) sampler: vk::Sampler,
}

impl Program {
    pub(super) fn owner(&self) -> &Arc<MemoryOwner> {
        &self.owner
    }
    pub(in crate::backends::vulkan) fn new(
        owner: Arc<MemoryOwner>,
        format: vk::Format,
    ) -> Result<Arc<Self>, RuntimeError> {
        let mut program = Self {
            owner,
            format,
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            sampler: vk::Sampler::null(),
        };
        let device = program.owner.device.raw();
        // Device creation retained these immutable physical limits.
        let limits = program.owner.device.limits();
        if limits.max_per_stage_descriptor_samplers < 1
            || limits.max_per_stage_descriptor_sampled_images < 1
            || limits.max_descriptor_set_samplers < 1
            || limits.max_descriptor_set_sampled_images < 1
            || limits.max_color_attachments < 1
        {
            return Err(RuntimeError::unsupported(
                "presentation shader ABI exceeds Device limits",
            ));
        }
        let bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        // SAFETY: All creation arrays remain live. Each successful handle is
        // immediately owned by program, including every partial failure.
        unsafe {
            program.set_layout = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
                .map_err(|r| native(&program.owner, "vkCreateDescriptorSetLayout(present)", r))?;
            program.layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&[program.set_layout]),
                    None,
                )
                .map_err(|r| native(&program.owner, "vkCreatePipelineLayout(present)", r))?;
            program.sampler = device
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::NEAREST)
                        .min_filter(vk::Filter::NEAREST)
                        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .max_lod(0.0),
                    None,
                )
                .map_err(|r| native(&program.owner, "vkCreateSampler(present)", r))?;
        }
        let vertex = Module::new(
            Arc::clone(&program.owner),
            include_bytes!("../../../../shaders/composite.vert.spv"),
        )?;
        let fragment = Module::new(
            Arc::clone(&program.owner),
            include_bytes!("../../../../shaders/composite.frag.spv"),
        )?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vertex.handle)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment.handle)
                .name(c"main"),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&states);
        let formats = [format];
        let mut rendering =
            vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
        let info = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(program.layout)
            .push_next(&mut rendering)];
        // SAFETY: The fixed composite shaders and descriptor layout agree.
        // The SRGB attachment format was checked against Surface capabilities.
        match unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &info, None) } {
            Ok(values) => program.pipeline = values[0],
            Err((partial, result)) => {
                for pipeline in partial {
                    // SAFETY: Partially created pipelines have no recorded uses.
                    unsafe { device.destroy_pipeline(pipeline, None) };
                }
                return Err(native(
                    &program.owner,
                    "vkCreateGraphicsPipelines(present)",
                    result,
                ));
            }
        }
        Ok(Arc::new(program))
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        // SAFETY: Slot/work owners retain this program through GPU completion.
        // Null handles make partial initialization use the same cleanup path.
        unsafe {
            let device = self.owner.device.raw();
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_sampler(self.sampler, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

struct Module {
    owner: Arc<MemoryOwner>,
    handle: vk::ShaderModule,
}
impl Module {
    fn new(owner: Arc<MemoryOwner>, bytes: &[u8]) -> Result<Self, RuntimeError> {
        let words = ash::util::read_spv(&mut std::io::Cursor::new(bytes))
            .map_err(|e| RuntimeError::invalid(format!("invalid presentation SPIR-V: {e}")))?;
        // SAFETY: Checked-in trusted SPIR-V validated by the shader build gate.
        let handle = unsafe {
            owner
                .device
                .raw()
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        }
        .map_err(|r| native(&owner, "vkCreateShaderModule(present)", r))?;
        Ok(Self { owner, handle })
    }
}
impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: Pipeline creation consumed the module; no command references it.
        unsafe {
            self.owner
                .device
                .raw()
                .destroy_shader_module(self.handle, None)
        };
    }
}

pub(super) struct Binding {
    pub(super) program: Arc<Program>,
    pool: vk::DescriptorPool,
    pub(super) set: vk::DescriptorSet,
}
impl Binding {
    pub(super) fn new(program: Arc<Program>) -> Result<Self, RuntimeError> {
        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)];
        // SAFETY: Program and Device are live; this one-set pool is owned below.
        let pool = unsafe {
            program.owner.device.raw().create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(1)
                    .pool_sizes(&sizes),
                None,
            )
        }
        .map_err(|r| native(&program.owner, "vkCreateDescriptorPool(present)", r))?;
        let mut binding = Self {
            program,
            pool,
            set: vk::DescriptorSet::null(),
        };
        let layouts = [binding.program.set_layout];
        // SAFETY: This exclusively owned pool has capacity for the exact layout.
        binding.set = unsafe {
            binding.program.owner.device.raw().allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&layouts),
            )
        }
        .map_err(|r| {
            native(
                &binding.program.owner,
                "vkAllocateDescriptorSets(present)",
                r,
            )
        })?[0];
        Ok(binding)
    }
    pub(super) fn update(&mut self, view: vk::ImageView) {
        let images = [vk::DescriptorImageInfo::default()
            .sampler(self.program.sampler)
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [vk::WriteDescriptorSet::default()
            .dst_set(self.set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&images)];
        // SAFETY: Caller holds the unique idle Slot (all prior GPU references
        // retired). Its new work payload retains the source image and this set.
        unsafe {
            self.program
                .owner
                .device
                .raw()
                .update_descriptor_sets(&writes, &[])
        };
    }
}
impl Drop for Binding {
    fn drop(&mut self) {
        // SAFETY: Binding is retained with its Slot until recorded work retires.
        unsafe {
            self.program
                .owner
                .device
                .raw()
                .destroy_descriptor_pool(self.pool, None)
        };
    }
}

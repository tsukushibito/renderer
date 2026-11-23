//! Fixed shader ABI validation, native creation and partial-failure ownership.

use std::sync::Arc;

use ash::vk;

use super::super::error::RuntimeError;
use super::super::memory::MemoryOwner;
use super::{RASTER_GROUP_COUNT, native};
use crate::program::{
    BindingType, BlendState, CompareOp, ManagedRasterProgram, ShaderStages, VertexFormat,
};

pub(super) fn shader_stages(stages: ShaderStages) -> vk::ShaderStageFlags {
    let mut native = vk::ShaderStageFlags::empty();
    if stages.contains(ShaderStages::VERTEX) {
        native |= vk::ShaderStageFlags::VERTEX;
    }
    if stages.contains(ShaderStages::FRAGMENT) {
        native |= vk::ShaderStageFlags::FRAGMENT;
    }
    native
}

fn vertex_format(format: VertexFormat) -> vk::Format {
    match format {
        VertexFormat::Float32x2 => vk::Format::R32G32_SFLOAT,
        VertexFormat::Float32x3 => vk::Format::R32G32B32_SFLOAT,
        VertexFormat::Float32x4 => vk::Format::R32G32B32A32_SFLOAT,
    }
}

pub(super) fn descriptor_type(kind: BindingType) -> vk::DescriptorType {
    match kind {
        BindingType::Uniform => vk::DescriptorType::UNIFORM_BUFFER,
        BindingType::StorageRead => vk::DescriptorType::STORAGE_BUFFER,
        BindingType::SampledImage => vk::DescriptorType::SAMPLED_IMAGE,
        BindingType::Sampler => vk::DescriptorType::SAMPLER,
        _ => unreachable!("trusted Raster read-only layout"),
    }
}

fn compare_op(compare: CompareOp) -> vk::CompareOp {
    match compare {
        CompareOp::Never => vk::CompareOp::NEVER,
        CompareOp::Less => vk::CompareOp::LESS,
        CompareOp::Equal => vk::CompareOp::EQUAL,
        CompareOp::LessEqual => vk::CompareOp::LESS_OR_EQUAL,
        CompareOp::Greater => vk::CompareOp::GREATER,
        CompareOp::NotEqual => vk::CompareOp::NOT_EQUAL,
        CompareOp::GreaterEqual => vk::CompareOp::GREATER_OR_EQUAL,
        CompareOp::Always => vk::CompareOp::ALWAYS,
    }
}

struct ShaderModule {
    owner: Arc<MemoryOwner>,
    module: vk::ShaderModule,
}

impl ShaderModule {
    fn new(owner: Arc<MemoryOwner>, bytes: &[u8]) -> Result<Self, RuntimeError> {
        let words = spirv_words(bytes)?;
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
        // SAFETY: Reviewed built-ins or caller-trusted shader assets enter here;
        // words are aligned and stay live through synchronous module creation.
        let module = unsafe {
            owner
                .device
                .raw()
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        }
        .map_err(|result| native(&owner, "vkCreateShaderModule(Graph)", result))?;
        Ok(Self { owner, module })
    }
}

fn spirv_words(bytes: &[u8]) -> Result<Vec<u32>, RuntimeError> {
    if !bytes.len().is_multiple_of(4) || bytes.len() < 20 {
        return Err(RuntimeError::invalid("invalid Raster SPIR-V word length"));
    }
    let words: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect();
    if words[0] != 0x0723_0203 {
        return Err(RuntimeError::invalid("invalid Raster SPIR-V magic"));
    }
    Ok(words)
}

impl Drop for ShaderModule {
    fn drop(&mut self) {
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectDestroy, 1);
        // SAFETY: Module creation use ends with synchronous pipeline creation;
        // pipelines do not retain a lifetime dependency on ShaderModule.
        unsafe {
            self.owner
                .device
                .raw()
                .destroy_shader_module(self.module, None)
        };
    }
}

/// One immutable program owns its complete native dependency chain. Null fields
/// permit immediate ownership and cleanup of partially initialized programs.
pub(in crate::backends::vulkan) struct Program {
    owner: Arc<MemoryOwner>,
    pub(super) layout: vk::PipelineLayout,
    pub(super) pipeline: vk::Pipeline,
    multisample_pipeline: vk::Pipeline,
    pub(in crate::backends::vulkan) sampler: vk::Sampler,
    linear_sampler: vk::Sampler,
    linear_clamp_sampler: vk::Sampler,
    linear_u_repeat_sampler: vk::Sampler,
    pub(super) raster_layouts: [vk::DescriptorSetLayout; RASTER_GROUP_COUNT],
    pub(super) raster_groups: usize,
    pub(in crate::backends::vulkan) raster_asset: ManagedRasterProgram,
    pub(super) raster_depth: bool,
}

impl Program {
    pub(in crate::backends::vulkan) fn raster_pipeline(
        &self,
        sample_count: crate::graph::SampleCount,
    ) -> Option<vk::Pipeline> {
        let pipeline = match sample_count {
            crate::graph::SampleCount::X1 => self.pipeline,
            crate::graph::SampleCount::X4 => self.multisample_pipeline,
        };
        (pipeline != vk::Pipeline::null()).then_some(pipeline)
    }

    pub(in crate::backends::vulkan) fn sampler_for(
        &self,
        desc: crate::program::SamplerDescriptor,
    ) -> vk::Sampler {
        match desc {
            crate::program::SamplerDescriptor::NearestClamp => self.sampler,
            crate::program::SamplerDescriptor::LinearRepeat => self.linear_sampler,
            crate::program::SamplerDescriptor::LinearClamp => self.linear_clamp_sampler,
            crate::program::SamplerDescriptor::LinearRepeatUClampV => self.linear_u_repeat_sampler,
        }
    }

    /// Explicit preparation for a reviewed Raster variant. Native handles are
    /// owned immediately, including partial layout/pipeline failures.
    pub(super) fn new_raster(
        owner: Arc<MemoryOwner>,
        managed: &ManagedRasterProgram,
        depth: bool,
        samples: vk::SampleCountFlags,
    ) -> Result<Self, RuntimeError> {
        if managed.requires_depth() && !depth {
            return Err(RuntimeError::invalid(
                "Raster Program requires a depth attachment",
            ));
        }
        let limits = owner.device.limits();
        if (!managed.is_depth_only() && !limits.framebuffer_color_sample_counts.contains(samples))
            || (depth && !limits.framebuffer_depth_sample_counts.contains(samples))
        {
            return Err(RuntimeError::unsupported(
                "Device does not support requested Raster framebuffer sample counts",
            ));
        }
        let groups = managed
            .bindings()
            .iter()
            .map(|slot| slot.group as usize + 1)
            .max()
            .unwrap_or(0);
        if limits.max_push_constants_size < managed.parameter_size()
            || limits.max_vertex_input_attributes < managed.vertex_attributes().len() as u32
            || (!managed.is_fullscreen()
                && limits.max_vertex_input_bindings
                    < 1 + u32::from(managed.descriptor().secondary_vertex_stride != 0))
            || limits.max_vertex_input_binding_stride
                < managed
                    .vertex_stride()
                    .max(managed.descriptor().secondary_vertex_stride)
            || managed.vertex_attributes().iter().any(|attribute| {
                attribute.offset > limits.max_vertex_input_attribute_offset
                    || attribute.location >= limits.max_vertex_input_attributes
            })
            || managed.color_outputs().len() > crate::program::MAX_RASTER_COLOR_ATTACHMENTS
            || managed.color_outputs().len() > limits.max_color_attachments as usize
            || managed.color_outputs().len() > limits.max_fragment_output_attachments as usize
            || limits.max_bound_descriptor_sets < groups as u32
        {
            return Err(RuntimeError::unsupported(
                "Device does not support Raster ABI limits",
            ));
        }
        for stage in [ShaderStages::VERTEX, ShaderStages::FRAGMENT] {
            let mut counts = [0; 4];
            for slot in managed
                .bindings()
                .iter()
                .filter(|slot| slot.stages.contains(stage))
            {
                counts[match slot.kind {
                    BindingType::Uniform => 0,
                    BindingType::StorageRead => 1,
                    BindingType::SampledImage => 2,
                    BindingType::Sampler => 3,
                    _ => unreachable!("trusted Raster layout"),
                }] += 1;
            }
            if counts[0] > limits.max_per_stage_descriptor_uniform_buffers
                || counts[1] > limits.max_per_stage_descriptor_storage_buffers
                || counts[2] > limits.max_per_stage_descriptor_sampled_images
                || counts[3] > limits.max_per_stage_descriptor_samplers
                || counts[..3].iter().sum::<u32>() > limits.max_per_stage_resources
            {
                return Err(RuntimeError::unsupported(
                    "Raster descriptors exceed Device per-stage limits",
                ));
            }
        }
        let mut totals = [0; 4];
        for slot in managed.bindings() {
            totals[match slot.kind {
                BindingType::Uniform => 0,
                BindingType::StorageRead => 1,
                BindingType::SampledImage => 2,
                BindingType::Sampler => 3,
                _ => unreachable!("trusted Raster layout"),
            }] += 1;
            let maximum = match slot.kind {
                BindingType::Uniform => u64::from(limits.max_uniform_buffer_range),
                BindingType::StorageRead => u64::from(limits.max_storage_buffer_range),
                _ => u64::MAX,
            };
            if slot.min_size > maximum {
                return Err(RuntimeError::unsupported(
                    "Raster buffer minimum exceeds Device range",
                ));
            }
        }
        if totals[0] > limits.max_descriptor_set_uniform_buffers
            || totals[1] > limits.max_descriptor_set_storage_buffers
            || totals[2] > limits.max_descriptor_set_sampled_images
            || totals[3] > limits.max_descriptor_set_samplers
        {
            return Err(RuntimeError::unsupported(
                "Raster descriptors exceed Device pipeline limits",
            ));
        }
        let query = |format,
                     features: vk::FormatFeatureFlags,
                     buffer|
         -> Result<(), RuntimeError> {
            if format == vk::Format::UNDEFINED {
                return Ok(());
            }
            // SAFETY: The retained Device owns this live physical device and Instance.
            let properties = unsafe {
                owner
                    .device
                    .instance_raw()
                    .get_physical_device_format_properties(owner.device.physical_device(), format)
            };
            let available = if buffer {
                properties.buffer_features
            } else {
                properties.optimal_tiling_features
            };
            if !available.contains(features) {
                return Err(RuntimeError::unsupported(
                    "Device lacks Raster format support",
                ));
            }
            Ok(())
        };
        for attribute in managed.vertex_attributes() {
            query(
                vertex_format(attribute.format),
                vk::FormatFeatureFlags::VERTEX_BUFFER,
                true,
            )?;
        }
        let color_features = vk::FormatFeatureFlags::COLOR_ATTACHMENT
            | if managed.blend() != BlendState::Replace {
                vk::FormatFeatureFlags::COLOR_ATTACHMENT_BLEND
            } else {
                vk::FormatFeatureFlags::empty()
            };
        for &format in managed.color_outputs() {
            query(
                super::super::resources::image_format(format),
                color_features,
                false,
            )?;
        }
        if depth {
            query(
                vk::Format::D32_SFLOAT,
                vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT,
                false,
            )?;
        }
        let mut result = Self {
            owner,
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            multisample_pipeline: vk::Pipeline::null(),
            sampler: vk::Sampler::null(),
            linear_sampler: vk::Sampler::null(),
            linear_clamp_sampler: vk::Sampler::null(),
            linear_u_repeat_sampler: vk::Sampler::null(),
            raster_layouts: [vk::DescriptorSetLayout::null(); RASTER_GROUP_COUNT],
            raster_groups: groups,
            raster_asset: managed.clone(),
            raster_depth: depth,
        };
        for group in 0..groups {
            let bindings: Vec<_> = managed
                .bindings()
                .iter()
                .filter(|slot| slot.group as usize == group)
                .map(|slot| {
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(slot.binding)
                        .descriptor_type(descriptor_type(slot.kind))
                        .descriptor_count(1)
                        .stage_flags(shader_stages(slot.stages))
                })
                .collect();
            #[cfg(feature = "memory-profiling")]
            crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
            // SAFETY: Trusted manifest slots are unique and Device limits were checked;
            // every created layout is owned before another native operation can fail.
            result.raster_layouts[group] = unsafe {
                result.owner.device.raw().create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
            }
            .map_err(|r| native(&result.owner, "vkCreateDescriptorSetLayout(Raster)", r))?;
        }
        if managed
            .bindings()
            .iter()
            .any(|slot| slot.kind == BindingType::Sampler)
        {
            // SAFETY: Fixed nearest/clamp sampler requires no optional feature; result owns it.
            result.sampler = unsafe {
                result.owner.device.create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::NEAREST)
                        .min_filter(vk::Filter::NEAREST)
                        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .max_lod(0.0),
                )?
            };
            // SAFETY: Linear repeat requires no optional features. Images paired
            // with it are checked for linear-filter format support at preparation.
            result.linear_sampler = unsafe {
                result
                    .owner
                    .device
                    .create_sampler(&super::super::managed::sampler_info(
                        crate::program::SamplerDescriptor::LinearRepeat,
                    ))?
            };
            // SAFETY: Feature-free policies; the result immediately owns each handle.
            result.linear_clamp_sampler = unsafe {
                result
                    .owner
                    .device
                    .create_sampler(&super::super::managed::sampler_info(
                        crate::program::SamplerDescriptor::LinearClamp,
                    ))?
            };
            // SAFETY: Same ownership and feature contract as the other prepared samplers.
            result.linear_u_repeat_sampler = unsafe {
                result
                    .owner
                    .device
                    .create_sampler(&super::super::managed::sampler_info(
                        crate::program::SamplerDescriptor::LinearRepeatUClampV,
                    ))?
            };
        }
        let pushes = [vk::PushConstantRange {
            stage_flags: shader_stages(managed.parameter_layout().stages),
            offset: 0,
            size: managed.parameter_size(),
        }];
        let mut info =
            vk::PipelineLayoutCreateInfo::default().set_layouts(&result.raster_layouts[..groups]);
        if managed.parameter_size() != 0 {
            info = info.push_constant_ranges(&pushes);
        }
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
        // SAFETY: All layouts are retained by result; ABI limits checked above.
        result.layout = unsafe {
            result
                .owner
                .device
                .raw()
                .create_pipeline_layout(&info, None)
        }
        .map_err(|r| native(&result.owner, "vkCreatePipelineLayout(Raster)", r))?;
        if samples.contains(vk::SampleCountFlags::TYPE_1) {
            result.pipeline = result.graphics_pipeline(vk::SampleCountFlags::TYPE_1)?;
        }
        if samples.contains(vk::SampleCountFlags::TYPE_4) {
            result.multisample_pipeline = result.graphics_pipeline(vk::SampleCountFlags::TYPE_4)?;
        }
        Ok(result)
    }

    fn graphics_pipeline(
        &self,
        samples: vk::SampleCountFlags,
    ) -> Result<vk::Pipeline, RuntimeError> {
        let program = &self.raster_asset;
        let depth_attached = self.raster_depth;
        let depth_state = program.depth();
        let depth_enabled = depth_attached && depth_state.compare().is_some();
        let mut formats = [vk::Format::UNDEFINED; crate::program::MAX_RASTER_COLOR_ATTACHMENTS];
        let outputs = program.color_outputs();
        for (destination, &format) in formats.iter_mut().zip(outputs) {
            *destination = super::super::resources::image_format(format);
        }
        let color_count = outputs.len();
        let shaders = program
            .spirv()
            .ok_or_else(|| RuntimeError::unsupported("Raster Program has no SPIR-V shaders"))?;
        let vertex = ShaderModule::new(Arc::clone(&self.owner), shaders.vertex)?;
        let fragment = shaders
            .fragment
            .map(|bytes| ShaderModule::new(Arc::clone(&self.owner), bytes))
            .transpose()?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vertex.module)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(
                    fragment
                        .as_ref()
                        .map_or(vk::ShaderModule::null(), |module| module.module),
                )
                .name(c"main"),
        ];
        let stages = if fragment.is_none() {
            &stages[..1]
        } else {
            &stages[..]
        };
        let strides = [
            program.vertex_stride(),
            program.descriptor().secondary_vertex_stride,
        ];
        let bindings = [0, 1].map(|binding| vk::VertexInputBindingDescription {
            binding,
            stride: strides[binding as usize],
            input_rate: vk::VertexInputRate::VERTEX,
        });
        let attributes: Vec<_> = program
            .vertex_attributes()
            .iter()
            .map(|attribute| vk::VertexInputAttributeDescription {
                location: attribute.location,
                binding: attribute.stream,
                format: vertex_format(attribute.format),
                offset: attribute.offset,
            })
            .collect();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(if program.is_fullscreen() {
                &[]
            } else if strides[1] == 0 {
                &bindings[..1]
            } else {
                &bindings
            })
            .vertex_attribute_descriptions(&attributes);
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
        let multisample =
            vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(samples);
        let depth = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(depth_enabled)
            .depth_write_enable(depth_enabled && depth_state.write_enabled())
            .depth_compare_op(
                depth_state
                    .compare()
                    .map_or(vk::CompareOp::ALWAYS, compare_op),
            );
        let mode = program.blend();
        let (source, destination, source_alpha, destination_alpha) = match mode {
            BlendState::Replace => (
                vk::BlendFactor::ONE,
                vk::BlendFactor::ZERO,
                vk::BlendFactor::ONE,
                vk::BlendFactor::ZERO,
            ),
            BlendState::Alpha => (
                vk::BlendFactor::SRC_ALPHA,
                vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
                vk::BlendFactor::ONE,
                vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
            ),
            BlendState::PremultipliedAlpha => (
                vk::BlendFactor::ONE,
                vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
                vk::BlendFactor::ONE,
                vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
            ),
            BlendState::Additive => (
                vk::BlendFactor::ONE,
                vk::BlendFactor::ONE,
                vk::BlendFactor::ONE,
                vk::BlendFactor::ONE,
            ),
        };
        // Each output uses the same state, so the optional independentBlend
        // feature is not required.
        let color_attachment = vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(mode != BlendState::Replace)
            .src_color_blend_factor(source)
            .dst_color_blend_factor(destination)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(source_alpha)
            .dst_alpha_blend_factor(destination_alpha)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA);
        let color_attachments = [color_attachment; crate::program::MAX_RASTER_COLOR_ATTACHMENTS];
        let blend = vk::PipelineColorBlendStateCreateInfo::default()
            .attachments(&color_attachments[..color_count]);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&formats[..color_count])
            .depth_attachment_format(if depth_attached {
                vk::Format::D32_SFLOAT
            } else {
                vk::Format::UNDEFINED
            });
        let infos = [vk::GraphicsPipelineCreateInfo::default()
            .stages(stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(self.layout)
            .push_next(&mut rendering)];
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
        // SAFETY: All pointers refer to local live arrays and the fixed shader
        // ABI; Dynamic Rendering1.3 and the queried attachment formats are used.
        match unsafe {
            self.owner.device.raw().create_graphics_pipelines(
                vk::PipelineCache::null(),
                &infos,
                None,
            )
        } {
            Ok(pipelines) => Ok(pipelines[0]),
            Err((partial, error)) => {
                for pipeline in partial {
                    if pipeline == vk::Pipeline::null() {
                        continue;
                    }
                    #[cfg(feature = "memory-profiling")]
                    crate::measurement::record_native(
                        crate::measurement::NativeEvent::ObjectDestroy,
                        1,
                    );
                    // SAFETY: Returned partial pipelines have no recorded uses.
                    unsafe { self.owner.device.raw().destroy_pipeline(pipeline, None) };
                }
                Err(native(
                    &self.owner,
                    "vkCreateGraphicsPipelines(Graph)",
                    error,
                ))
            }
        }
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        // SAFETY: Published programs are owned by ShaderPrograms until all work
        // retires. Unpublished partial programs have never been recorded.
        unsafe {
            let device = self.owner.device.raw();
            for pipeline in [self.pipeline, self.multisample_pipeline] {
                if pipeline != vk::Pipeline::null() {
                    #[cfg(feature = "memory-profiling")]
                    crate::measurement::record_native(
                        crate::measurement::NativeEvent::ObjectDestroy,
                        1,
                    );
                    device.destroy_pipeline(pipeline, None);
                }
            }
            if self.layout != vk::PipelineLayout::null() {
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ObjectDestroy,
                    1,
                );
                device.destroy_pipeline_layout(self.layout, None);
            }
            for layout in self.raster_layouts {
                if layout != vk::DescriptorSetLayout::null() {
                    #[cfg(feature = "memory-profiling")]
                    crate::measurement::record_native(
                        crate::measurement::NativeEvent::ObjectDestroy,
                        1,
                    );
                    device.destroy_descriptor_set_layout(layout, None);
                }
            }
            self.owner.device.destroy_sampler(self.sampler);
            self.owner.device.destroy_sampler(self.linear_sampler);
            self.owner.device.destroy_sampler(self.linear_clamp_sampler);
            self.owner
                .device
                .destroy_sampler(self.linear_u_repeat_sampler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_shader_bytes_are_decoded_without_alignment_assumptions() {
        let bytes = include_bytes!("../../../../shaders/mesh.vert.spv");
        let mut unaligned = vec![0xff];
        unaligned.extend_from_slice(bytes);
        let words = spirv_words(&unaligned[1..]).unwrap();
        assert_eq!(words[0], 0x0723_0203);
        assert_eq!(
            words[1], 0x0001_0300,
            "fixed SPIR-V1.3 avoids optional maintenance4 LocalSizeId"
        );
        for malformed in [&bytes[..bytes.len() - 1], &bytes[..16], &[0u8; 20][..]] {
            assert!(spirv_words(malformed).is_err());
        }
    }
}

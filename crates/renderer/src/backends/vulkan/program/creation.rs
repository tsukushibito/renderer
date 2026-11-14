//! Fixed shader ABI validation, native creation and partial-failure ownership.

#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::{CStr, CString};
use std::sync::Arc;

use ash::vk;

use super::super::error::RuntimeError;
use super::super::memory::MemoryOwner;
#[cfg(feature = "vulkan-diagnostics")]
use super::name_object;
use super::native;

struct ShaderModule {
    owner: Arc<MemoryOwner>,
    module: vk::ShaderModule,
}

impl ShaderModule {
    fn new(owner: Arc<MemoryOwner>, bytes: &[u8]) -> Result<Self, RuntimeError> {
        let words = spirv_words(bytes)?;
        // SAFETY: Only fixed, validated, repository-owned shaders enter here;
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
        return Err(RuntimeError::invalid("invalid embedded SPIR-V word length"));
    }
    let words: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect();
    if words[0] != 0x0723_0203 {
        return Err(RuntimeError::invalid("invalid embedded SPIR-V magic"));
    }
    Ok(words)
}

impl Drop for ShaderModule {
    fn drop(&mut self) {
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

#[derive(Clone, Copy, Debug)]
pub(super) enum ProgramKind {
    Compute,
    Raster,
    Composite,
}

impl ProgramKind {
    #[cfg(feature = "vulkan-diagnostics")]
    fn name(self) -> &'static CStr {
        match self {
            Self::Compute => c"m2-image-compute",
            Self::Raster => c"m2-indexed-mesh-depth",
            Self::Composite => c"m2-sampled-composite",
        }
    }
}

/// Validate the ABI used by this operation before any of its native objects are
/// created. The query boundary also lets CPU tests model an adapter lacking D32
/// without substituting a fake implementation of the Vulkan object API.
fn validate_program(
    kind: ProgramKind,
    limits: &vk::PhysicalDeviceLimits,
    mut query: impl FnMut(vk::Format) -> vk::FormatProperties,
) -> Result<(), RuntimeError> {
    let supported = match kind {
        ProgramKind::Compute => {
            limits.max_compute_work_group_size[0] >= 8
                && limits.max_compute_work_group_size[1] >= 8
                && limits.max_compute_work_group_size[2] >= 1
                && limits.max_compute_work_group_invocations >= 64
                && limits.max_push_constants_size >= 4
                && limits.max_per_stage_descriptor_storage_images >= 2
                && limits.max_descriptor_set_storage_images >= 2
        }
        ProgramKind::Raster => {
            limits.max_push_constants_size >= 80
                && limits.max_vertex_input_attributes >= 2
                && limits.max_vertex_input_bindings >= 1
                && limits.max_vertex_input_binding_stride >= 28
                && limits.max_vertex_input_attribute_offset >= 12
                && limits.max_color_attachments >= 1
        }
        ProgramKind::Composite => {
            limits.max_per_stage_descriptor_samplers >= 1
                && limits.max_per_stage_descriptor_sampled_images >= 1
                && limits.max_descriptor_set_samplers >= 1
                && limits.max_descriptor_set_sampled_images >= 1
                && limits.max_color_attachments >= 1
        }
    };
    if !supported {
        return Err(RuntimeError::unsupported(format!(
            "adapter limits do not support the fixed {kind:?} shader ABI"
        )));
    }
    let mut require = |format, features: vk::FormatFeatureFlags, buffer| {
        let properties = query(format);
        let available = if buffer {
            properties.buffer_features
        } else {
            properties.optimal_tiling_features
        };
        if available.contains(features) {
            Ok(())
        } else {
            Err(RuntimeError::unsupported(format!(
                "{kind:?} requires format {format:?} features {features:?}; available {available:?}"
            )))
        }
    };
    match kind {
        ProgramKind::Compute => require(
            vk::Format::R8G8B8A8_UNORM,
            vk::FormatFeatureFlags::STORAGE_IMAGE,
            false,
        ),
        ProgramKind::Raster => {
            require(
                vk::Format::R8G8B8A8_UNORM,
                vk::FormatFeatureFlags::COLOR_ATTACHMENT,
                false,
            )?;
            // D32 is not mandatory on every Vulkan 1.3 adapter. Checking only
            // Device image creation misses graphs that do not even use depth.
            require(
                vk::Format::D32_SFLOAT,
                vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT,
                false,
            )?;
            for format in [
                vk::Format::R32G32B32_SFLOAT,
                vk::Format::R32G32B32A32_SFLOAT,
            ] {
                require(format, vk::FormatFeatureFlags::VERTEX_BUFFER, true)?;
            }
            Ok(())
        }
        ProgramKind::Composite => require(
            vk::Format::R8G8B8A8_UNORM,
            vk::FormatFeatureFlags::COLOR_ATTACHMENT | vk::FormatFeatureFlags::SAMPLED_IMAGE,
            false,
        ),
    }
}

/// One immutable program owns its complete native dependency chain. Null fields
/// permit immediate ownership and cleanup of partially initialized programs.
pub(super) struct Program {
    owner: Arc<MemoryOwner>,
    pub(super) set_layout: vk::DescriptorSetLayout,
    pub(super) layout: vk::PipelineLayout,
    pub(super) pipeline: vk::Pipeline,
    pub(super) sampler: vk::Sampler,
}

impl Program {
    pub(super) fn new(
        owner: Arc<MemoryOwner>,
        kind: ProgramKind,
        limits: &vk::PhysicalDeviceLimits,
    ) -> Result<Self, RuntimeError> {
        validate_program(kind, limits, |format| {
            // SAFETY: The retained Device owns this live adapter and Instance.
            unsafe {
                owner
                    .device
                    .instance_raw()
                    .get_physical_device_format_properties(owner.device.physical_device(), format)
            }
        })?;
        let mut result = Self {
            owner,
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            sampler: vk::Sampler::null(),
        };
        result.initialize(kind)?;
        Ok(result)
    }

    fn initialize(&mut self, kind: ProgramKind) -> Result<(), RuntimeError> {
        let device = self.owner.device.raw();
        let compute_bindings = [0, 1].map(|binding| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(binding)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        });
        let composite_bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let bindings: &[_] = match kind {
            ProgramKind::Compute => &compute_bindings,
            ProgramKind::Raster => &[],
            ProgramKind::Composite => &composite_bindings,
        };
        // SAFETY: Capability checks precede native creation. Every successful
        // handle is immediately owned here, including on failure or unwind.
        unsafe {
            if !bindings.is_empty() {
                self.set_layout = device
                    .create_descriptor_set_layout(
                        &vk::DescriptorSetLayoutCreateInfo::default().bindings(bindings),
                        None,
                    )
                    .map_err(|r| native(&self.owner, "vkCreateDescriptorSetLayout(Graph)", r))?;
            }
            let layouts = [self.set_layout];
            let pushes = [vk::PushConstantRange {
                stage_flags: if matches!(kind, ProgramKind::Raster) {
                    vk::ShaderStageFlags::VERTEX
                } else {
                    vk::ShaderStageFlags::COMPUTE
                },
                offset: 0,
                size: if matches!(kind, ProgramKind::Raster) {
                    80
                } else {
                    4
                },
            }];
            let mut layout_info = vk::PipelineLayoutCreateInfo::default();
            if !bindings.is_empty() {
                layout_info = layout_info.set_layouts(&layouts);
            }
            if matches!(kind, ProgramKind::Compute | ProgramKind::Raster) {
                layout_info = layout_info.push_constant_ranges(&pushes);
            }
            self.layout = device
                .create_pipeline_layout(&layout_info, None)
                .map_err(|r| native(&self.owner, "vkCreatePipelineLayout(Graph)", r))?;
            if matches!(kind, ProgramKind::Composite) {
                self.sampler = self.owner.device.create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::NEAREST)
                        .min_filter(vk::Filter::NEAREST)
                        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .max_lod(0.0),
                )?;
            }
        }
        self.pipeline = match kind {
            ProgramKind::Compute => self.compute_pipeline()?,
            ProgramKind::Raster => self.graphics_pipeline(true)?,
            ProgramKind::Composite => self.graphics_pipeline(false)?,
        };
        #[cfg(feature = "vulkan-diagnostics")]
        {
            let name = kind.name();
            name_object(&self.owner, self.pipeline, name)?;
            let layout_name = CString::new(format!("{}-layout", name.to_string_lossy()))
                .expect("fixed program names contain no NUL");
            name_object(&self.owner, self.layout, &layout_name)?;
            if !bindings.is_empty() {
                let set_name = CString::new(format!("{}-set-layout", name.to_string_lossy()))
                    .expect("fixed program names contain no NUL");
                name_object(&self.owner, self.set_layout, &set_name)?;
            }
            if matches!(kind, ProgramKind::Composite) {
                name_object(&self.owner, self.sampler, c"m2-composite-nearest-sampler")?;
            }
        }
        Ok(())
    }

    fn compute_pipeline(&self) -> Result<vk::Pipeline, RuntimeError> {
        let compute = ShaderModule::new(
            Arc::clone(&self.owner),
            include_bytes!("../../../../shaders/image.comp.spv"),
        )?;
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(compute.module)
            .name(c"main");
        let info = [vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(self.layout)];
        // SAFETY: Fixed module and layout ABI agree. Local arrays remain live
        // through creation. Partial pipeline successes are destroyed on failure.
        match unsafe {
            self.owner
                .device
                .raw()
                .create_compute_pipelines(vk::PipelineCache::null(), &info, None)
        } {
            Ok(pipelines) => Ok(pipelines[0]),
            Err((partial, error)) => {
                for pipeline in partial {
                    // SAFETY: Returned partial pipelines have no recorded uses.
                    unsafe { self.owner.device.raw().destroy_pipeline(pipeline, None) };
                }
                Err(native(
                    &self.owner,
                    "vkCreateComputePipelines(Graph)",
                    error,
                ))
            }
        }
    }
    fn graphics_pipeline(&self, mesh: bool) -> Result<vk::Pipeline, RuntimeError> {
        let vertex = ShaderModule::new(
            Arc::clone(&self.owner),
            if mesh {
                include_bytes!("../../../../shaders/mesh.vert.spv")
            } else {
                include_bytes!("../../../../shaders/composite.vert.spv")
            },
        )?;
        let fragment = ShaderModule::new(
            Arc::clone(&self.owner),
            if mesh {
                include_bytes!("../../../../shaders/mesh.frag.spv")
            } else {
                include_bytes!("../../../../shaders/composite.frag.spv")
            },
        )?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vertex.module)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment.module)
                .name(c"main"),
        ];
        let bindings = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: 28,
            input_rate: vk::VertexInputRate::VERTEX,
        }];
        let attributes = [
            vk::VertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: vk::Format::R32G32B32_SFLOAT,
                offset: 0,
            },
            vk::VertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: vk::Format::R32G32B32A32_SFLOAT,
                offset: 12,
            },
        ];
        let mut vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        if mesh {
            vertex_input = vertex_input
                .vertex_binding_descriptions(&bindings)
                .vertex_attribute_descriptions(&attributes);
        }
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
        let depth = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(mesh)
            .depth_write_enable(mesh)
            .depth_compare_op(vk::CompareOp::LESS);
        let color_attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&color_attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let formats = [vk::Format::R8G8B8A8_UNORM];
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&formats)
            .depth_attachment_format(if mesh {
                vk::Format::D32_SFLOAT
            } else {
                vk::Format::UNDEFINED
            });
        let infos = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
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
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
            self.owner.device.destroy_sampler(self.sampler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_capabilities_only_require_the_requested_operation() {
        // A compute-only adapter contract has no graphics ABI requirements.
        let compute_limits = vk::PhysicalDeviceLimits {
            max_compute_work_group_size: [8, 8, 1],
            max_compute_work_group_invocations: 64,
            max_push_constants_size: 4,
            max_per_stage_descriptor_storage_images: 2,
            max_descriptor_set_storage_images: 2,
            ..Default::default()
        };
        validate_program(ProgramKind::Compute, &compute_limits, |format| {
            assert_eq!(format, vk::Format::R8G8B8A8_UNORM);
            vk::FormatProperties {
                optimal_tiling_features: vk::FormatFeatureFlags::STORAGE_IMAGE,
                ..Default::default()
            }
        })
        .unwrap();

        // A sampled composite must also work without compute or D32 support.
        let composite_limits = vk::PhysicalDeviceLimits {
            max_per_stage_descriptor_samplers: 1,
            max_per_stage_descriptor_sampled_images: 1,
            max_descriptor_set_samplers: 1,
            max_descriptor_set_sampled_images: 1,
            max_color_attachments: 1,
            ..Default::default()
        };
        validate_program(ProgramKind::Composite, &composite_limits, |format| {
            assert_eq!(format, vk::Format::R8G8B8A8_UNORM);
            vk::FormatProperties {
                optimal_tiling_features: vk::FormatFeatureFlags::COLOR_ATTACHMENT
                    | vk::FormatFeatureFlags::SAMPLED_IMAGE,
                ..Default::default()
            }
        })
        .unwrap();
    }

    #[test]
    fn raster_rejects_missing_depth_or_color_support_before_native_creation() {
        let limits = vk::PhysicalDeviceLimits {
            max_push_constants_size: 80,
            max_vertex_input_attributes: 2,
            max_vertex_input_bindings: 1,
            max_vertex_input_binding_stride: 28,
            max_vertex_input_attribute_offset: 12,
            max_color_attachments: 1,
            ..Default::default()
        };
        for unsupported in [vk::Format::R8G8B8A8_UNORM, vk::Format::D32_SFLOAT] {
            let mut queried = Vec::new();
            let error = validate_program(ProgramKind::Raster, &limits, |format| {
                queried.push(format);
                if format == unsupported {
                    return vk::FormatProperties::default();
                }
                vk::FormatProperties {
                    optimal_tiling_features: vk::FormatFeatureFlags::COLOR_ATTACHMENT
                        | vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT,
                    buffer_features: vk::FormatFeatureFlags::VERTEX_BUFFER,
                    ..Default::default()
                }
            })
            .unwrap_err();
            assert_eq!(
                error.kind(),
                crate::backends::vulkan::error::RuntimeErrorKind::Unsupported
            );
            assert!(error.message.contains(&format!("{unsupported:?}")));
            assert_eq!(queried.last(), Some(&unsupported));
        }
    }

    #[test]
    fn trusted_shader_bytes_are_decoded_without_alignment_assumptions() {
        let bytes = include_bytes!("../../../../shaders/image.comp.spv");
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

//! Native pipeline and layout ownership for Graph Compute programs.

use super::error::RuntimeError;
use super::memory::MemoryOwner;
use crate::program::{BindingType, ManagedProgram};
use ash::vk;
use std::ffi::CString;
use std::sync::Arc;

mod sampler;
pub(super) use sampler::sampler_info;

fn native(owner: &MemoryOwner, operation: &str, result: vk::Result) -> RuntimeError {
    owner.device.observe_result(result);
    RuntimeError::native(operation, owner.device.name(), result)
}
pub(super) fn descriptor_type(kind: BindingType) -> vk::DescriptorType {
    match kind {
        BindingType::Uniform => vk::DescriptorType::UNIFORM_BUFFER,
        BindingType::StorageRead | BindingType::StorageWrite => vk::DescriptorType::STORAGE_BUFFER,
        BindingType::SampledImage => vk::DescriptorType::SAMPLED_IMAGE,
        BindingType::StorageImageRead(_) | BindingType::StorageImageWrite(_) => {
            vk::DescriptorType::STORAGE_IMAGE
        }
        BindingType::Sampler => vk::DescriptorType::SAMPLER,
    }
}

pub(super) struct ComputePipeline {
    owner: Arc<MemoryOwner>,
    pub(super) set_layouts: Vec<vk::DescriptorSetLayout>,
    pub(super) layout: vk::PipelineLayout,
    pub(super) pipeline: vk::Pipeline,
}

impl ComputePipeline {
    pub(super) fn create(
        owner: Arc<MemoryOwner>,
        managed: &ManagedProgram,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let bindings = managed.bindings();
        let uniform_count = bindings
            .iter()
            .filter(|b| b.kind == BindingType::Uniform)
            .count() as u32;
        let storage_count = bindings
            .iter()
            .filter(|b| matches!(b.kind, BindingType::StorageRead | BindingType::StorageWrite))
            .count() as u32;
        let group_count = bindings
            .iter()
            .map(|b| b.group.checked_add(1))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| RuntimeError::unsupported("Compute descriptor group count overflow"))?
            .into_iter()
            .max()
            .unwrap_or(0);
        let image_count = bindings
            .iter()
            .filter(|b| b.kind == BindingType::SampledImage)
            .count() as u32;
        let sampler_count = bindings
            .iter()
            .filter(|b| b.kind == BindingType::Sampler)
            .count() as u32;
        let storage_image_count = bindings
            .iter()
            .filter(|b| {
                matches!(
                    b.kind,
                    BindingType::StorageImageRead(_) | BindingType::StorageImageWrite(_)
                )
            })
            .count() as u32;
        let local = managed.workgroup_size();
        let limits = owner.device.limits();
        let invocations = local.into_iter().try_fold(1_u32, u32::checked_mul);
        if limits.max_bound_descriptor_sets < group_count
            || limits.max_per_stage_descriptor_uniform_buffers < uniform_count
            || limits.max_descriptor_set_uniform_buffers < uniform_count
            || limits.max_per_stage_descriptor_storage_buffers < storage_count
            || limits.max_descriptor_set_storage_buffers < storage_count
            || limits.max_per_stage_descriptor_sampled_images < image_count
            || limits.max_descriptor_set_sampled_images < image_count
            || limits.max_per_stage_descriptor_storage_images < storage_image_count
            || limits.max_descriptor_set_storage_images < storage_image_count
            || limits.max_per_stage_descriptor_samplers < sampler_count
            || limits.max_descriptor_set_samplers < sampler_count
            || limits.max_per_stage_resources
                < uniform_count + storage_count + image_count + storage_image_count
            || bindings.iter().any(|b| {
                b.min_size
                    > match b.kind {
                        BindingType::Uniform => u64::from(limits.max_uniform_buffer_range),
                        BindingType::StorageRead | BindingType::StorageWrite => {
                            u64::from(limits.max_storage_buffer_range)
                        }
                        BindingType::SampledImage
                        | BindingType::Sampler
                        | BindingType::StorageImageRead(_)
                        | BindingType::StorageImageWrite(_) => 0,
                    }
            })
            || limits
                .max_compute_work_group_count
                .into_iter()
                .any(|count| count == 0)
            || local
                .into_iter()
                .zip(limits.max_compute_work_group_size)
                .any(|(size, limit)| size == 0 || size > limit)
            || invocations.is_none_or(|count| count > limits.max_compute_work_group_invocations)
        {
            return Err(RuntimeError::unsupported(
                "Device does not support managed program ABI",
            ));
        }
        for slot in bindings {
            if let BindingType::StorageImageRead(format) | BindingType::StorageImageWrite(format) =
                slot.kind
            {
                let format = match format {
                    crate::ImageFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
                    crate::ImageFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
                    _ => {
                        return Err(RuntimeError::unsupported(
                            "storage images require Rgba8Unorm or Rgba16Float",
                        ));
                    }
                };
                // SAFETY: Device retains this physical adapter and Instance.
                let properties = unsafe {
                    owner
                        .device
                        .instance_raw()
                        .get_physical_device_format_properties(
                            owner.device.physical_device(),
                            format,
                        )
                };
                if !properties
                    .optimal_tiling_features
                    .contains(vk::FormatFeatureFlags::STORAGE_IMAGE)
                {
                    return Err(RuntimeError::unsupported(
                        "Compute storage image format is unsupported",
                    ));
                }
            }
        }
        let mut program = Self {
            owner,
            set_layouts: Vec::new(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
        };
        program.initialize(managed, group_count)?;
        Ok(program)
    }

    fn initialize(
        &mut self,
        managed: &ManagedProgram,
        group_count: u32,
    ) -> Result<(), RuntimeError> {
        let bytes = managed
            .spirv()
            .ok_or_else(|| RuntimeError::unsupported("Compute Program has no SPIR-V shader"))?;
        let entry = CString::new(managed.entry_point())
            .map_err(|_| RuntimeError::invalid("Compute entry point contains NUL"))?;
        if bytes.len() < 20 || !bytes.len().is_multiple_of(4) {
            return Err(RuntimeError::invalid("invalid managed SPIR-V length"));
        }
        let words = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect::<Vec<_>>();
        if words[0] != 0x0723_0203 {
            return Err(RuntimeError::invalid("invalid managed SPIR-V magic"));
        }
        let device = self.owner.device.raw();
        for group in 0..group_count {
            let bindings = managed
                .bindings()
                .iter()
                .filter(|b| b.group == group)
                .map(|b| {
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(b.binding)
                        .descriptor_type(descriptor_type(b.kind))
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::COMPUTE)
                })
                .collect::<Vec<_>>();
            #[cfg(feature = "memory-profiling")]
            crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
            // SAFETY: The reviewed manifest describes this validated binary.
            // The local table remains live through synchronous creation.
            let layout = unsafe {
                device.create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
            }
            .map_err(|r| native(&self.owner, "vkCreateDescriptorSetLayout(managed)", r))?;
            self.set_layouts.push(layout);
        }
        // SAFETY: All inputs describe the reviewed managed ABI. Every native
        // result is owned immediately for partial-failure cleanup.
        unsafe {
            #[cfg(feature = "memory-profiling")]
            crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
            self.layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&self.set_layouts),
                    None,
                )
                .map_err(|r| native(&self.owner, "vkCreatePipelineLayout(managed)", r))?;
            #[cfg(feature = "memory-profiling")]
            crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
            let module = ShaderModule {
                owner: &self.owner,
                handle: device
                    .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
                    .map_err(|r| native(&self.owner, "vkCreateShaderModule(managed)", r))?,
            };
            let infos = [vk::ComputePipelineCreateInfo::default()
                .layout(self.layout)
                .stage(
                    vk::PipelineShaderStageCreateInfo::default()
                        .stage(vk::ShaderStageFlags::COMPUTE)
                        .module(module.handle)
                        .name(&entry),
                )];
            #[cfg(feature = "memory-profiling")]
            crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
            self.pipeline =
                match device.create_compute_pipelines(vk::PipelineCache::null(), &infos, None) {
                    Ok(pipelines) => pipelines[0],
                    Err((partial, result)) => {
                        for pipeline in partial {
                            if pipeline == vk::Pipeline::null() {
                                continue;
                            }
                            #[cfg(feature = "memory-profiling")]
                            crate::measurement::record_native(
                                crate::measurement::NativeEvent::ObjectDestroy,
                                1,
                            );
                            device.destroy_pipeline(pipeline, None);
                        }
                        return Err(native(
                            &self.owner,
                            "vkCreateComputePipelines(managed)",
                            result,
                        ));
                    }
                };
        }
        Ok(())
    }
}
impl Drop for ComputePipeline {
    fn drop(&mut self) {
        // SAFETY: Binding/recording owners retain all pipeline dependencies.
        // Drop occurs before recording, after cancel or actual GPU retirement.
        unsafe {
            let device = self.owner.device.raw();
            if self.pipeline != vk::Pipeline::null() {
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ObjectDestroy,
                    1,
                );
                device.destroy_pipeline(self.pipeline, None);
            }
            if self.layout != vk::PipelineLayout::null() {
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ObjectDestroy,
                    1,
                );
                device.destroy_pipeline_layout(self.layout, None);
            }
            for layout in &self.set_layouts {
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ObjectDestroy,
                    1,
                );
                device.destroy_descriptor_set_layout(*layout, None);
            }
        }
    }
}

struct ShaderModule<'a> {
    owner: &'a MemoryOwner,
    handle: vk::ShaderModule,
}
impl Drop for ShaderModule<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectDestroy, 1);
        // SAFETY: Synchronous pipeline creation no longer uses this module.
        unsafe {
            self.owner
                .device
                .raw()
                .destroy_shader_module(self.handle, None)
        };
    }
}

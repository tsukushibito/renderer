//! Managed programs and immutable uniform bindings, independent of Graph state.

use std::sync::Arc;

use ash::vk;
use vk_mem::Alloc;

use crate::program::{ManagedProgram, UniformBindingDesc, UniformLayout, validate_uniform_binding};

use super::buffer::host_access::{self, VmaAccess};
use super::error::RuntimeError;
use super::memory::MemoryOwner;

fn native(owner: &MemoryOwner, operation: &str, result: vk::Result) -> RuntimeError {
    owner.device.observe_result(result);
    RuntimeError::native(operation, owner.device.name(), result)
}

/// An immutable managed compute pipeline and its complete layout ownership.
/// Creating another program never modifies this version.
#[derive(Clone)]
pub struct VulkanProgram(Arc<Program>);

impl VulkanProgram {
    pub fn uniform_layout(&self) -> UniformLayout {
        self.0.uniform
    }

    pub(super) fn create(
        owner: Arc<MemoryOwner>,
        managed: &ManagedProgram,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let uniform = managed.uniform_layout();
        let local = managed.workgroup_size();
        let limits = owner.device.limits();
        let invocations = local.into_iter().try_fold(1_u32, u32::checked_mul);
        if uniform.group != 0
            || uniform.binding != 0
            || limits.max_bound_descriptor_sets < 2
            || limits.max_per_stage_descriptor_uniform_buffers < 1
            || limits.max_descriptor_set_uniform_buffers < 1
            || limits.max_per_stage_descriptor_storage_buffers < 1
            || limits.max_descriptor_set_storage_buffers < 1
            || limits.max_per_stage_resources < 2
            || u64::from(limits.max_uniform_buffer_range) < uniform.min_size
            || limits.max_storage_buffer_range < 16
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
        let mut program = Program {
            owner,
            uniform,
            uniform_set: vk::DescriptorSetLayout::null(),
            output_set: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
        };
        program.initialize(managed.spirv())?;
        Ok(Self(Arc::new(program)))
    }
}

struct Program {
    owner: Arc<MemoryOwner>,
    uniform: UniformLayout,
    uniform_set: vk::DescriptorSetLayout,
    // The managed shader's output is exercised only by the acceptance fixture;
    // general owned dispatch/output bindings belong to the next Graph slice.
    output_set: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

struct ShaderModule<'a> {
    owner: &'a MemoryOwner,
    handle: vk::ShaderModule,
}
impl Drop for ShaderModule<'_> {
    fn drop(&mut self) {
        // SAFETY: Synchronous pipeline creation no longer uses this module.
        unsafe {
            self.owner
                .device
                .raw()
                .destroy_shader_module(self.handle, None)
        };
    }
}

impl Program {
    fn initialize(&mut self, bytes: &[u8]) -> Result<(), RuntimeError> {
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
        let uniform = [vk::DescriptorSetLayoutBinding::default()
            .binding(self.uniform.binding)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE)];
        let output = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE)];
        // SAFETY: The managed registry ties this fixed ABI to validated binary
        // contents. All inputs remain live through synchronous creation and
        // every successful handle is immediately owned for partial cleanup.
        unsafe {
            self.uniform_set = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&uniform),
                    None,
                )
                .map_err(|r| {
                    native(
                        &self.owner,
                        "vkCreateDescriptorSetLayout(managed uniform)",
                        r,
                    )
                })?;
            self.output_set = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&output),
                    None,
                )
                .map_err(|r| {
                    native(
                        &self.owner,
                        "vkCreateDescriptorSetLayout(managed output)",
                        r,
                    )
                })?;
            let layouts = [self.uniform_set, self.output_set];
            self.layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts),
                    None,
                )
                .map_err(|r| native(&self.owner, "vkCreatePipelineLayout(managed)", r))?;
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
                        .name(c"main"),
                )];
            self.pipeline =
                match device.create_compute_pipelines(vk::PipelineCache::null(), &infos, None) {
                    Ok(pipelines) => pipelines[0],
                    Err((partial, result)) => {
                        for pipeline in partial {
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

impl Drop for Program {
    fn drop(&mut self) {
        // SAFETY: Every binding/recording retains this owner. Destruction occurs
        // before recording, after cancellation, or after actual GPU retirement.
        unsafe {
            let device = self.owner.device.raw();
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.output_set, None);
            device.destroy_descriptor_set_layout(self.uniform_set, None);
        }
    }
}

/// Uniform bytes are copied once during creation, then remain immutable.
/// An update is a new buffer; previous bindings retain their original bytes.
#[derive(Clone)]
pub struct VulkanUniformBuffer(Arc<UniformBuffer>);

impl VulkanUniformBuffer {
    pub fn len(&self) -> u64 {
        self.0.size
    }
    pub fn is_empty(&self) -> bool {
        false
    }

    pub(super) fn create(owner: Arc<MemoryOwner>, bytes: &[u8]) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let size = bytes.len() as u64;
        if size == 0 || size > owner.device.max_buffer_size() {
            return Err(RuntimeError::invalid(
                "uniform buffer size must be nonzero and within maxBufferSize",
            ));
        }
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::UNIFORM_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let allocation_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferHost,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            ..Default::default()
        };
        // SAFETY: Exact nonzero request, retained VMA/Device, no escaped mapping.
        let (handle, allocation) = unsafe { owner.raw.create_buffer(&info, &allocation_info) }
            .map_err(|r| native(&owner, "vmaCreateBuffer(uniform)", r))?;
        let mut buffer = UniformBuffer {
            owner,
            handle,
            allocation,
            size,
        };
        let info = buffer.owner.raw.get_allocation_info(&buffer.allocation);
        // SAFETY: The retained allocator owns this immutable properties table.
        let properties = unsafe { buffer.owner.raw.get_memory_properties() };
        let coherent = properties.memory_types[info.memory_type as usize]
            .property_flags
            .contains(vk::MemoryPropertyFlags::HOST_COHERENT);
        let mut access = VmaAccess::new(&buffer.owner.raw, &mut buffer.allocation);
        // SAFETY: This newly created allocation is exclusively owned and has no
        // GPU references. The input slice fits its exact logical byte length.
        unsafe { host_access::write(&mut access, coherent, 0, bytes) }
            .map_err(|error| native(&buffer.owner, error.operation, error.result))?;
        Ok(Self(Arc::new(buffer)))
    }
}

struct UniformBuffer {
    owner: Arc<MemoryOwner>,
    handle: vk::Buffer,
    allocation: vk_mem::Allocation,
    size: u64,
}
impl Drop for UniformBuffer {
    fn drop(&mut self) {
        // SAFETY: Immutable bindings retain the allocation through every GPU
        // use; no mutable mapping or native handle escapes this owner.
        unsafe {
            self.owner
                .raw
                .destroy_buffer(self.handle, &mut self.allocation)
        };
    }
}

/// A descriptor set written once, retaining its program and uniform bytes.
#[derive(Clone)]
pub struct VulkanUniformBinding(Arc<UniformBinding>);

impl VulkanUniformBinding {
    pub fn desc(&self) -> UniformBindingDesc {
        self.0.desc
    }

    pub(super) fn create(
        owner: &Arc<MemoryOwner>,
        program: &VulkanProgram,
        buffer: &VulkanUniformBuffer,
        desc: UniformBindingDesc,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if !Arc::ptr_eq(owner, &program.0.owner) || !Arc::ptr_eq(owner, &buffer.0.owner) {
            return Err(RuntimeError::wrong_owner(
                "uniform binding program/buffer belongs to another Device",
            ));
        }
        let limits = owner.device.limits();
        validate_uniform_binding(
            program.0.uniform,
            buffer.len(),
            desc,
            limits.min_uniform_buffer_offset_alignment,
            u64::from(limits.max_uniform_buffer_range),
        )
        .map_err(|error| RuntimeError::invalid(error.to_string()))?;
        let mut binding = UniformBinding {
            program: program.clone(),
            _buffer: buffer.clone(),
            desc,
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::null(),
        };
        let sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: 1,
        }];
        // SAFETY: Fresh private pool/set, one host owner and no prior recorded
        // use. The checked descriptor range remains immutable for its lifetime.
        unsafe {
            binding.pool = owner
                .device
                .raw()
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(1)
                        .pool_sizes(&sizes),
                    None,
                )
                .map_err(|r| native(owner, "vkCreateDescriptorPool(uniform)", r))?;
            let layouts = [program.0.uniform_set];
            binding.set = owner
                .device
                .raw()
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(binding.pool)
                        .set_layouts(&layouts),
                )
                .map_err(|r| native(owner, "vkAllocateDescriptorSets(uniform)", r))?[0];
            let buffers = [vk::DescriptorBufferInfo::default()
                .buffer(buffer.0.handle)
                .offset(desc.offset)
                .range(desc.size)];
            let writes = [vk::WriteDescriptorSet::default()
                .dst_set(binding.set)
                .dst_binding(desc.binding)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&buffers)];
            owner.device.raw().update_descriptor_sets(&writes, &[]);
        }
        Ok(Self(Arc::new(binding)))
    }
}

struct UniformBinding {
    program: VulkanProgram,
    _buffer: VulkanUniformBuffer,
    desc: UniformBindingDesc,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
}
impl Drop for UniformBinding {
    fn drop(&mut self) {
        // SAFETY: Recorded/pending owners retain this immutable binding; pool
        // destruction precedes the retained program and uniform buffer release.
        unsafe {
            self.program
                .0
                .owner
                .device
                .raw()
                .destroy_descriptor_pool(self.pool, None)
        };
    }
}

#[cfg(test)]
mod tests;

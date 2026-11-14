//! Managed buffer resources and immutable bindings, independent of Graph state.

use super::buffer::host_access::{self, VmaAccess};
use super::error::RuntimeError;
use super::memory::MemoryOwner;
use crate::program::{
    BufferBinding, BufferBindingDesc, BufferBindingLayout, BufferBindingType, BufferDescriptor,
    BufferUsage, ManagedProgram, validate_binding_slots, validate_buffer_binding,
};
use ash::vk;
use std::sync::Arc;
use vk_mem::Alloc;

fn native(owner: &MemoryOwner, operation: &str, result: vk::Result) -> RuntimeError {
    owner.device.observe_result(result);
    RuntimeError::native(operation, owner.device.name(), result)
}
fn descriptor_type(kind: BufferBindingType) -> vk::DescriptorType {
    match kind {
        BufferBindingType::Uniform => vk::DescriptorType::UNIFORM_BUFFER,
        BufferBindingType::StorageWrite => vk::DescriptorType::STORAGE_BUFFER,
    }
}

/// An immutable managed compute pipeline and its complete layout ownership.
#[derive(Clone)]
pub struct VulkanProgram(Arc<Program>);
impl VulkanProgram {
    pub fn layout(&self) -> &'static [BufferBindingLayout] {
        self.0.bindings
    }
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
            .filter(|b| b.kind == BufferBindingType::Uniform)
            .count() as u32;
        let storage_count = bindings
            .iter()
            .filter(|b| b.kind == BufferBindingType::StorageWrite)
            .count() as u32;
        let group_count = bindings.iter().map(|b| b.group + 1).max().unwrap_or(0);
        let local = managed.workgroup_size();
        let limits = owner.device.limits();
        let invocations = local.into_iter().try_fold(1_u32, u32::checked_mul);
        if limits.max_bound_descriptor_sets < group_count
            || limits.max_per_stage_descriptor_uniform_buffers < uniform_count
            || limits.max_descriptor_set_uniform_buffers < uniform_count
            || limits.max_per_stage_descriptor_storage_buffers < storage_count
            || limits.max_descriptor_set_storage_buffers < storage_count
            || limits.max_per_stage_resources < uniform_count + storage_count
            || bindings.iter().any(|b| {
                b.min_size
                    > match b.kind {
                        BufferBindingType::Uniform => u64::from(limits.max_uniform_buffer_range),
                        BufferBindingType::StorageWrite => {
                            u64::from(limits.max_storage_buffer_range)
                        }
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
        let mut program = Program {
            owner,
            bindings,
            set_layouts: Vec::new(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
        };
        program.initialize(managed.spirv(), group_count)?;
        Ok(Self(Arc::new(program)))
    }
}
struct Program {
    owner: Arc<MemoryOwner>,
    bindings: &'static [BufferBindingLayout],
    set_layouts: Vec<vk::DescriptorSetLayout>,
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
    fn initialize(&mut self, bytes: &[u8], group_count: u32) -> Result<(), RuntimeError> {
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
            let bindings = self
                .bindings
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
            self.layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&self.set_layouts),
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
        // SAFETY: Binding/recording owners retain all pipeline dependencies.
        // Drop occurs before recording, after cancel or actual GPU retirement.
        unsafe {
            let device = self.owner.device.raw();
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            for layout in &self.set_layouts {
                device.destroy_descriptor_set_layout(*layout, None);
            }
        }
    }
}

/// An owned buffer with fixed usage and size. Uniform bytes are copied once;
/// storage contents may change through GPU work while descriptors stay immutable.
/// No native mapping or CPU mutation escapes this owner.
#[derive(Clone)]
pub struct VulkanBuffer(Arc<Buffer>);
impl VulkanBuffer {
    pub fn len(&self) -> u64 {
        self.0.size
    }
    pub fn is_empty(&self) -> bool {
        false
    }
    pub fn usage(&self) -> BufferUsage {
        self.0.usage
    }
    pub(super) fn create(
        owner: Arc<MemoryOwner>,
        desc: BufferDescriptor<'_>,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let (size, usage, initial) = match desc {
            BufferDescriptor::Uniform { bytes } => {
                (bytes.len() as u64, BufferUsage::Uniform, Some(bytes))
            }
            BufferDescriptor::Storage { size } => (size, BufferUsage::Storage, None),
        };
        if size == 0 || size > owner.device.max_buffer_size() {
            return Err(RuntimeError::invalid(
                "managed buffer size must be nonzero and within maxBufferSize",
            ));
        }
        let flags = match usage {
            BufferUsage::Uniform => vk::BufferUsageFlags::UNIFORM_BUFFER,
            BufferUsage::Storage => {
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC
            }
        };
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(flags)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let allocation_info = match usage {
            BufferUsage::Uniform => vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferHost,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            BufferUsage::Storage => vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferDevice,
                ..Default::default()
            },
        };
        // SAFETY: Exact nonzero request, retained VMA/Device, no escaped mapping.
        let (handle, allocation) = unsafe { owner.raw.create_buffer(&info, &allocation_info) }
            .map_err(|r| native(&owner, "vmaCreateBuffer(managed)", r))?;
        let mut buffer = Buffer {
            owner,
            handle,
            allocation,
            size,
            usage,
            #[cfg(test)]
            storage: StorageState::default(),
        };
        if let Some(bytes) = initial {
            let info = buffer.owner.raw.get_allocation_info(&buffer.allocation);
            // SAFETY: The retained allocator owns this immutable properties table.
            let properties = unsafe { buffer.owner.raw.get_memory_properties() };
            let coherent = properties.memory_types[info.memory_type as usize]
                .property_flags
                .contains(vk::MemoryPropertyFlags::HOST_COHERENT);
            let mut access = VmaAccess::new(&buffer.owner.raw, &mut buffer.allocation);
            // SAFETY: Fresh exclusive allocation, exact addressable byte length,
            // no GPU references. The scoped map is flushed before publication.
            unsafe { host_access::write(&mut access, coherent, 0, bytes) }
                .map_err(|error| native(&buffer.owner, error.operation, error.result))?;
        }
        Ok(Self(Arc::new(buffer)))
    }
}
struct Buffer {
    owner: Arc<MemoryOwner>,
    handle: vk::Buffer,
    allocation: vk_mem::Allocation,
    size: u64,
    usage: BufferUsage,
    // Only the private acceptance executor runs these resources today. Its
    // actual exclusion/state stays out of production until general execution.
    #[cfg(test)]
    storage: StorageState,
}
#[cfg(test)]
#[derive(Default)]
struct StorageState {
    busy: std::sync::atomic::AtomicBool,
    used: std::sync::atomic::AtomicBool,
}
impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: Bindings/GPU owners retain the allocation through every
        // recorded use, completion or cancellation; no mapping escapes.
        unsafe {
            self.owner
                .raw
                .destroy_buffer(self.handle, &mut self.allocation)
        };
    }
}

/// Descriptor sets written once, retaining all bound buffers and the Program.
#[derive(Clone)]
pub struct VulkanBindings(Arc<Bindings>);
impl VulkanBindings {
    /// Binding descriptors are in the Program manifest's canonical order.
    pub fn descs(&self) -> &[BufferBindingDesc] {
        &self.0.descs
    }
    pub(super) fn create(
        owner: &Arc<MemoryOwner>,
        program: &VulkanProgram,
        input: &[BufferBinding<'_, VulkanBuffer>],
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if !Arc::ptr_eq(owner, &program.0.owner)
            || input.iter().any(|b| !Arc::ptr_eq(owner, &b.buffer.0.owner))
        {
            return Err(RuntimeError::wrong_owner(
                "managed Program/buffer belongs to another Device",
            ));
        }
        let requested = input.iter().map(|b| b.desc).collect::<Vec<_>>();
        validate_binding_slots(program.0.bindings, &requested)
            .map_err(|e| RuntimeError::invalid(e.to_string()))?;
        let limits = owner.device.limits();
        let mut buffers = Vec::with_capacity(input.len());
        let mut descs = Vec::with_capacity(input.len());
        for layout in program.0.bindings {
            let binding = input
                .iter()
                .find(|b| b.desc.group == layout.group && b.desc.binding == layout.binding)
                .unwrap();
            let (alignment, maximum) = match layout.kind {
                BufferBindingType::Uniform => (
                    limits.min_uniform_buffer_offset_alignment,
                    limits.max_uniform_buffer_range,
                ),
                BufferBindingType::StorageWrite => (
                    limits.min_storage_buffer_offset_alignment,
                    limits.max_storage_buffer_range,
                ),
            };
            validate_buffer_binding(
                *layout,
                binding.buffer.usage(),
                binding.buffer.len(),
                binding.desc,
                alignment,
                u64::from(maximum),
            )
            .map_err(|e| RuntimeError::invalid(e.to_string()))?;
            buffers.push(binding.buffer.clone());
            descs.push(binding.desc);
        }
        let mut bindings = Bindings {
            program: program.clone(),
            buffers,
            descs,
            pool: vk::DescriptorPool::null(),
            sets: Vec::new(),
        };
        let sizes = [BufferBindingType::Uniform, BufferBindingType::StorageWrite]
            .into_iter()
            .filter_map(|kind| {
                let count = program.0.bindings.iter().filter(|b| b.kind == kind).count() as u32;
                (count != 0).then_some(vk::DescriptorPoolSize {
                    ty: descriptor_type(kind),
                    descriptor_count: count,
                })
            })
            .collect::<Vec<_>>();
        // SAFETY: Fresh pool/sets, checked logical ranges and correct native
        // resource usages. Every referenced owner is retained before update;
        // descriptor contents stay unchanged through all later GPU use.
        unsafe {
            bindings.pool = owner
                .device
                .raw()
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(program.0.set_layouts.len() as u32)
                        .pool_sizes(&sizes),
                    None,
                )
                .map_err(|r| native(owner, "vkCreateDescriptorPool(managed)", r))?;
            bindings.sets = owner
                .device
                .raw()
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(bindings.pool)
                        .set_layouts(&program.0.set_layouts),
                )
                .map_err(|r| native(owner, "vkAllocateDescriptorSets(managed)", r))?;
            let infos = bindings
                .buffers
                .iter()
                .zip(&bindings.descs)
                .map(|(buffer, desc)| {
                    vk::DescriptorBufferInfo::default()
                        .buffer(buffer.0.handle)
                        .offset(desc.offset)
                        .range(desc.size)
                })
                .collect::<Vec<_>>();
            let writes = program
                .0
                .bindings
                .iter()
                .zip(&infos)
                .map(|(layout, info)| {
                    vk::WriteDescriptorSet::default()
                        .dst_set(bindings.sets[layout.group as usize])
                        .dst_binding(layout.binding)
                        .descriptor_type(descriptor_type(layout.kind))
                        .buffer_info(std::slice::from_ref(info))
                })
                .collect::<Vec<_>>();
            owner.device.raw().update_descriptor_sets(&writes, &[]);
        }
        Ok(Self(Arc::new(bindings)))
    }
}
struct Bindings {
    program: VulkanProgram,
    buffers: Vec<VulkanBuffer>,
    descs: Vec<BufferBindingDesc>,
    pool: vk::DescriptorPool,
    sets: Vec<vk::DescriptorSet>,
}
impl Drop for Bindings {
    fn drop(&mut self) {
        // SAFETY: Execution owners retain these immutable sets until actual
        // retirement/cancel. Destroy the pool before Program and buffers drop.
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

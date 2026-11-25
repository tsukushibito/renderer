//! Explicitly prepared managed Compute pipelines and exclusive descriptor arenas.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::sync::{Arc, Mutex};

use ash::vk;

use super::{PhysicalLease, RuntimeError, native};
use crate::backends::vulkan::{
    managed::{ComputePipeline, descriptor_type},
    memory::MemoryOwner,
};
use crate::graph::BindingOperands;
use crate::program::{BindingType, ManagedProgram, ProgramId};

struct PreparedProgram {
    managed: ManagedProgram,
    native: ComputePipeline,
}

struct Arena {
    owner: Arc<MemoryOwner>,
    pool: vk::DescriptorPool,
    sets: Vec<Vec<vk::DescriptorSet>>,
    used: usize,
}

impl Arena {
    fn new(
        owner: Arc<MemoryOwner>,
        programs: &[PreparedProgram],
        capacity: u32,
    ) -> Result<Self, RuntimeError> {
        let types = [
            vk::DescriptorType::UNIFORM_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::SAMPLED_IMAGE,
            vk::DescriptorType::STORAGE_IMAGE,
            vk::DescriptorType::SAMPLER,
        ];
        let mut sizes = Vec::new();
        for ty in types {
            let count = programs
                .iter()
                .flat_map(|program| program.managed.bindings())
                .filter(|slot| descriptor_type(slot.kind) == ty)
                .count();
            let count = u32::try_from(count)
                .ok()
                .and_then(|count| count.checked_mul(capacity))
                .ok_or_else(|| {
                    RuntimeError::capacity_with(
                        "Compute descriptor count overflow",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::Descriptors,
                            CapacityRecovery::Reconfigure,
                            None,
                            None,
                        ),
                    )
                })?;
            if count != 0 {
                sizes.push(vk::DescriptorPoolSize {
                    ty,
                    descriptor_count: count,
                });
            }
        }
        let group_count = programs.iter().try_fold(0u32, |total, program| {
            total.checked_add(program.native.set_layouts.len() as u32)
        });
        let max_sets = group_count
            .and_then(|count| count.checked_mul(capacity))
            .ok_or_else(|| {
                RuntimeError::capacity_with(
                    "Compute descriptor set count overflow",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Descriptors,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
        let mut result = Self {
            owner,
            pool: vk::DescriptorPool::null(),
            sets: Vec::with_capacity(programs.len()),
            used: 0,
        };
        if max_sets != 0 {
            #[cfg(feature = "memory-profiling")]
            crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
            // SAFETY: Counts were checked; this new pool is uniquely owned before
            // allocation and remains retained through every recorded use.
            result.pool = unsafe {
                result.owner.device.raw().create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(max_sets)
                        .pool_sizes(&sizes),
                    None,
                )
            }
            .map_err(|r| native(&result.owner, "vkCreateDescriptorPool(owned Compute)", r))?;
        }
        for program in programs {
            let layouts: Vec<_> = (0..capacity)
                .flat_map(|_| program.native.set_layouts.iter().copied())
                .collect();
            let sets = if layouts.is_empty() {
                Vec::new()
            } else {
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::DescriptorAllocate,
                    layouts.len() as u64,
                );
                // SAFETY: The new pool has exactly the prepared descriptor and
                // set capacity; layouts remain live in the immutable program.
                unsafe {
                    result.owner.device.raw().allocate_descriptor_sets(
                        &vk::DescriptorSetAllocateInfo::default()
                            .descriptor_pool(result.pool)
                            .set_layouts(&layouts),
                    )
                }
                .map_err(|r| native(&result.owner, "vkAllocateDescriptorSets(owned Compute)", r))?
            };
            result.sets.push(sets);
        }
        Ok(result)
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        if self.pool != vk::DescriptorPool::null() {
            #[cfg(feature = "memory-profiling")]
            {
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::DescriptorFree,
                    self.sets.iter().map(Vec::len).sum::<usize>() as u64,
                );
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ObjectDestroy,
                    1,
                );
            }
            // SAFETY: Cancellation or actual GPU retirement precedes returning
            // an arena. Unresolved submissions retain their complete lease.
            unsafe {
                self.owner
                    .device
                    .raw()
                    .destroy_descriptor_pool(self.pool, None)
            };
        }
    }
}

struct Cache {
    idle: Vec<Arena>,
    leased: usize,
    reuses: usize,
}

pub(super) struct ComputePrograms {
    owner: Arc<MemoryOwner>,
    // Arenas are destroyed before their descriptor layouts and sampler.
    cache: Mutex<Cache>,
    programs: Vec<PreparedProgram>,
    capacity: usize,
}

impl ComputePrograms {
    pub(super) fn new(
        owner: Arc<MemoryOwner>,
        programs: &[ManagedProgram],
        slots: usize,
        capacity: u32,
    ) -> Result<Arc<Self>, RuntimeError> {
        let mut result = Self {
            owner,
            cache: Mutex::new(Cache {
                idle: Vec::with_capacity(slots),
                leased: 0,
                reuses: 0,
            }),
            programs: Vec::with_capacity(programs.len()),
            capacity: capacity as usize,
        };
        for managed in programs {
            if result.supports(managed.identity()) {
                continue;
            }
            let native = ComputePipeline::create(Arc::clone(&result.owner), managed)?;
            result.programs.push(PreparedProgram {
                managed: managed.clone(),
                native,
            });
        }
        for _ in 0..slots {
            let arena = Arena::new(Arc::clone(&result.owner), &result.programs, capacity)?;
            result.cache.get_mut().unwrap().idle.push(arena);
        }
        Ok(Arc::new(result))
    }

    pub(super) fn arena_stats(&self) -> (usize, usize) {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        (cache.idle.len() + cache.leased, cache.reuses)
    }

    pub(super) fn is_idle(&self) -> bool {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).leased == 0
    }

    pub(super) fn supports(&self, id: ProgramId) -> bool {
        self.program_index(id).is_some()
    }

    fn program_index(&self, id: ProgramId) -> Option<usize> {
        self.programs
            .iter()
            .position(|program| program.managed.identity() == id)
    }

    pub(super) fn acquire(self: &Arc<Self>) -> Result<ComputeLease, RuntimeError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| RuntimeError::invalid("Compute descriptor cache lock poisoned"))?;
        let arena = cache.idle.pop().ok_or_else(|| {
            RuntimeError::capacity_with(
                "Compute descriptor arenas exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Descriptors,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            )
        })?;
        cache.leased += 1;
        cache.reuses += 1;
        Ok(ComputeLease {
            programs: Arc::clone(self),
            arena: Some(arena),
        })
    }
}

impl Drop for ComputePrograms {
    fn drop(&mut self) {
        self.cache
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .idle
            .clear();
    }
}

#[derive(Clone, Copy)]
pub(super) struct ComputeDispatch {
    program: usize,
    invocation: usize,
}

pub(super) struct ComputeLease {
    programs: Arc<ComputePrograms>,
    arena: Option<Arena>,
}

impl ComputeLease {
    pub(super) fn prepare(
        &mut self,
        id: ProgramId,
        operands: &[BindingOperands],
        physical: &[Option<PhysicalLease>],
        workgroups: [u32; 3],
    ) -> Result<ComputeDispatch, RuntimeError> {
        let index = self.programs.program_index(id).ok_or_else(|| {
            RuntimeError::capacity_with(
                "Compute Program is not prepared",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Programs,
                    CapacityRecovery::Prepare,
                    None,
                    None,
                ),
            )
        })?;
        let limits = self.programs.owner.device.limits();
        validate_workgroups(workgroups, limits.max_compute_work_group_count)?;
        let arena = self.arena.as_mut().unwrap();
        if arena.used >= self.programs.capacity {
            return Err(RuntimeError::capacity_with(
                "Compute dispatch descriptor capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Descriptors,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            ));
        }
        let program = &self.programs.programs[index];
        let bindings = program.managed.bindings();
        super::work::validate_sampled_operands(bindings, operands, physical, None)?;
        let native_program = &program.native;
        let groups = native_program.set_layouts.len();
        let sets = &arena.sets[index][arena.used * groups..(arena.used + 1) * groups];
        for (slot, operand) in bindings.iter().zip(operands) {
            let mut buffer_info = vk::DescriptorBufferInfo::default();
            let mut image_info = vk::DescriptorImageInfo::default();
            let mut write = vk::WriteDescriptorSet::default()
                .dst_set(sets[slot.group as usize])
                .dst_binding(slot.binding)
                .descriptor_type(descriptor_type(slot.kind));
            match operand {
                BindingOperands::Buffer { resource, range } => {
                    let (alignment, maximum) = match slot.kind {
                        BindingType::Uniform => (
                            limits.min_uniform_buffer_offset_alignment,
                            limits.max_uniform_buffer_range,
                        ),
                        _ => (
                            limits.min_storage_buffer_offset_alignment,
                            limits.max_storage_buffer_range,
                        ),
                    };
                    if !range.offset.is_multiple_of(alignment) || range.size > u64::from(maximum) {
                        return Err(RuntimeError::unsupported(
                            "Compute buffer alignment/range exceeds Device limits",
                        ));
                    }
                    buffer_info.buffer = physical[*resource].as_ref().unwrap().buffer().handle;
                    buffer_info.offset = range.offset;
                    buffer_info.range = range.size;
                    write = write.buffer_info(std::slice::from_ref(&buffer_info));
                }
                BindingOperands::ImageView(view) => {
                    image_info.image_view = physical[view.image().index]
                        .as_ref()
                        .unwrap()
                        .image()
                        .view_for_mips(view.mip_level(), view.mip_level_count())
                        .expect("validated prepared image view range");
                    image_info.image_layout = match slot.kind {
                        BindingType::SampledImage => vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        _ => vk::ImageLayout::GENERAL,
                    };
                    write = write.image_info(std::slice::from_ref(&image_info));
                }
                BindingOperands::Sampler(desc) => {
                    image_info.sampler = self.programs.owner.device.sampler(*desc)?;
                    write = write.image_info(std::slice::from_ref(&image_info));
                }
            }
            // SAFETY: Canonical Graph bindings established types/ranges. This
            // arena is exclusive; each set is updated once before recording and
            // all native operands remain pinned through this lease's retirement.
            unsafe {
                self.programs
                    .owner
                    .device
                    .raw()
                    .update_descriptor_sets(&[write], &[])
            };
        }
        let dispatch = ComputeDispatch {
            program: index,
            invocation: arena.used,
        };
        arena.used += 1;
        Ok(dispatch)
    }

    /// # Safety
    /// Called outside rendering, after Graph dependencies and layouts were
    /// recorded, with this exclusive arena and all physical operands retained.
    pub(super) unsafe fn record(
        &self,
        command: vk::CommandBuffer,
        dispatch: ComputeDispatch,
        workgroups: [u32; 3],
    ) {
        let program = &self.programs.programs[dispatch.program].native;
        let groups = program.set_layouts.len();
        let sets = &self.arena.as_ref().unwrap().sets[dispatch.program]
            [dispatch.invocation * groups..(dispatch.invocation + 1) * groups];
        let device = self.programs.owner.device.raw();
        // SAFETY: The caller retains the pipeline/layout/sets and transitioned
        // operands; preparation checked Device dispatch counts and buffer limits.
        unsafe {
            device.cmd_bind_pipeline(command, vk::PipelineBindPoint::COMPUTE, program.pipeline);
            if !sets.is_empty() {
                device.cmd_bind_descriptor_sets(
                    command,
                    vk::PipelineBindPoint::COMPUTE,
                    program.layout,
                    0,
                    sets,
                    &[],
                );
            }
            device.cmd_dispatch(command, workgroups[0], workgroups[1], workgroups[2]);
        }
    }
}

impl Drop for ComputeLease {
    fn drop(&mut self) {
        let Some(mut arena) = self.arena.take() else {
            return;
        };
        arena.used = 0;
        let mut cache = self
            .programs
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.leased -= 1;
        cache.idle.push(arena);
    }
}

fn validate_workgroups(workgroups: [u32; 3], maximum: [u32; 3]) -> Result<(), RuntimeError> {
    if workgroups
        .into_iter()
        .zip(maximum)
        .any(|(count, maximum)| count == 0 || count > maximum)
    {
        return Err(RuntimeError::unsupported(
            "Compute dispatch exceeds Device workgroup counts",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_limits_apply_to_each_dimension_in_every_configuration() {
        assert!(validate_workgroups([7, 5, 3], [7, 5, 3]).is_ok());
        for workgroups in [
            [0, 1, 1],
            [1, 0, 1],
            [1, 1, 0],
            [8, 1, 1],
            [1, 6, 1],
            [1, 1, 4],
        ] {
            let error = validate_workgroups(workgroups, [7, 5, 3]).unwrap_err();
            assert_eq!(
                error.kind(),
                crate::backends::vulkan::error::RuntimeErrorKind::Unsupported
            );
        }
    }
}

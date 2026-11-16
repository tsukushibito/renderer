//! Descriptor arena ownership, bounded reuse and immutable binding leases.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::CString;
use std::sync::{Arc, Mutex};

use ash::vk;

use super::super::error::RuntimeError;
use super::super::memory::MemoryOwner;
use super::super::resources::DeviceImage;
use super::creation::descriptor_type;
#[cfg(feature = "vulkan-diagnostics")]
use super::name_object;
use super::{
    RASTER_GROUP_COUNT, RASTER_VARIANT_COUNT, RasterLayout, ShaderNeeds, ShaderPrograms, native,
};
use crate::program::{BindingLayout, BindingType};

pub(in crate::backends::vulkan) enum RasterBinding {
    Buffer {
        buffer: vk::Buffer,
        offset: u64,
        size: u64,
    },
    Image(vk::ImageView),
    Sampler(vk::Sampler),
}

struct DescriptorArena {
    owner: Arc<MemoryOwner>,
    pool: vk::DescriptorPool,
    compute: Vec<vk::DescriptorSet>,
    composite: Vec<vk::DescriptorSet>,
    raster: [Vec<[vk::DescriptorSet; RASTER_GROUP_COUNT]>; RASTER_VARIANT_COUNT],
}

impl DescriptorArena {
    fn new(
        owner: Arc<MemoryOwner>,
        max_bindings: u32,
        compute: Option<vk::DescriptorSetLayout>,
        composite: Option<vk::DescriptorSetLayout>,
        raster: [Option<RasterLayout>; RASTER_VARIANT_COUNT],
        #[cfg_attr(not(feature = "vulkan-diagnostics"), allow(unused_variables))] ordinal: usize,
    ) -> Result<Self, RuntimeError> {
        let types = [
            vk::DescriptorType::UNIFORM_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::SAMPLED_IMAGE,
            vk::DescriptorType::SAMPLER,
        ];
        let mut sizes = Vec::new();
        if compute.is_some() {
            sizes.push(vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_IMAGE,
                descriptor_count: max_bindings * 2,
            });
        }
        if composite.is_some() {
            sizes.push(vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: max_bindings,
            });
        }
        for (index, ty) in types.into_iter().enumerate() {
            let count: u32 = raster
                .iter()
                .flatten()
                .map(|layout| layout.descriptors[index])
                .sum();
            if count != 0 {
                sizes.push(vk::DescriptorPoolSize {
                    ty,
                    descriptor_count: count * max_bindings,
                });
            }
        }
        let raster_groups: u32 = raster
            .iter()
            .flatten()
            .map(|layout| layout.groups as u32)
            .sum();
        let kinds = u32::from(compute.is_some()) + u32::from(composite.is_some()) + raster_groups;
        if kinds == 0 {
            return Err(RuntimeError::invalid(
                "descriptor preparation requires a layout",
            ));
        }
        let info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(max_bindings * kinds)
            .pool_sizes(&sizes);
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
        // SAFETY: Counts checked at ShaderPrograms creation; this arena is
        // exclusively leased, and its pool remains live through recorded use.
        let pool = unsafe { owner.device.raw().create_descriptor_pool(&info, None) }
            .map_err(|result| native(&owner, "vkCreateDescriptorPool(Graph)", result))?;
        let mut result = Self {
            owner,
            pool,
            compute: Vec::new(),
            composite: Vec::new(),
            raster: std::array::from_fn(|_| Vec::new()),
        };
        for (layout, sets) in [
            (compute, &mut result.compute),
            (composite, &mut result.composite),
        ] {
            if let Some(layout) = layout {
                let layouts = vec![layout; max_bindings as usize];
                let allocation = vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&layouts);
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::DescriptorAllocate,
                    u64::from(max_bindings),
                );
                // SAFETY: Explicit preparation owns the fresh pool. Every set
                // remains allocated until arena destruction, including failures.
                *sets = unsafe {
                    result
                        .owner
                        .device
                        .raw()
                        .allocate_descriptor_sets(&allocation)
                }
                .map_err(|r| {
                    native(&result.owner, "vkAllocateDescriptorSets(Graph prepared)", r)
                })?;
            }
        }
        for (index, layouts) in raster.into_iter().enumerate() {
            let Some(layouts) = layouts else {
                continue;
            };
            let prepared: Vec<_> = (0..max_bindings)
                .flat_map(|_| layouts.layouts[..layouts.groups].iter().copied())
                .collect();
            let info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&prepared);
            #[cfg(feature = "memory-profiling")]
            crate::measurement::record_native(
                crate::measurement::NativeEvent::DescriptorAllocate,
                u64::from(max_bindings) * layouts.groups as u64,
            );
            // SAFETY: Fresh uniquely owned pool; sets survive through its exclusive lease.
            let sets = unsafe { result.owner.device.raw().allocate_descriptor_sets(&info) }
                .map_err(|r| {
                    native(
                        &result.owner,
                        "vkAllocateDescriptorSets(Raster prepared)",
                        r,
                    )
                })?;
            result.raster[index] = sets
                .chunks_exact(layouts.groups)
                .map(|chunk| {
                    let mut prepared = [vk::DescriptorSet::null(); RASTER_GROUP_COUNT];
                    prepared[..chunk.len()].copy_from_slice(chunk);
                    prepared
                })
                .collect();
        }
        #[cfg(feature = "vulkan-diagnostics")]
        {
            let name = CString::new(format!("m2-descriptor-arena-{ordinal}"))
                .expect("numeric name has no NUL");
            name_object(&result.owner, pool, &name)?;
        }
        Ok(result)
    }
}

impl Drop for DescriptorArena {
    fn drop(&mut self) {
        #[cfg(feature = "memory-profiling")]
        {
            crate::measurement::record_native(
                crate::measurement::NativeEvent::DescriptorFree,
                (self.compute.len()
                    + self.composite.len()
                    + self
                        .raster
                        .iter()
                        .flat_map(|sets| sets.iter())
                        .flatten()
                        .filter(|&&set| set != vk::DescriptorSet::null())
                        .count()) as u64,
            );
            crate::measurement::record_native(crate::measurement::NativeEvent::ObjectDestroy, 1);
        }
        // SAFETY: A checked-out arena cannot reach Drop before its executor
        // proves retirement (or cancellation); unknown completion retains it.
        unsafe {
            self.owner
                .device
                .raw()
                .destroy_descriptor_pool(self.pool, None)
        };
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::backends::vulkan) struct ShaderStats {
    pub(in crate::backends::vulkan) descriptor_arenas_created: usize,
    pub(in crate::backends::vulkan) descriptor_arenas_reused: usize,
    pub(in crate::backends::vulkan) descriptor_arenas_leased: usize,
}

struct DescriptorCache {
    idle: Vec<DescriptorArena>,
    stats: ShaderStats,
    layouts: (
        Option<vk::DescriptorSetLayout>,
        Option<vk::DescriptorSetLayout>,
        [Option<RasterLayout>; RASTER_VARIANT_COUNT],
    ),
}

impl Default for DescriptorCache {
    fn default() -> Self {
        Self {
            idle: Vec::new(),
            stats: ShaderStats::default(),
            layouts: (None, None, [None; RASTER_VARIANT_COUNT]),
        }
    }
}

/// Bounded descriptor allocation state, independent of program publication.
/// Stored inline so the responsibility split adds no allocation or lock.
pub(super) struct BindingPool {
    slots: usize,
    max_bindings: u32,
    cache: Mutex<DescriptorCache>,
}

impl BindingPool {
    pub(super) fn new(slots: usize, max_bindings: u32) -> Result<Self, RuntimeError> {
        if slots == 0
            || max_bindings == 0
            || max_bindings
                > u32::MAX
                    / (2 + crate::program::MAX_RASTER_BINDINGS as u32 * RASTER_VARIANT_COUNT as u32)
        {
            return Err(RuntimeError::invalid(
                "Graph shader capacities must be nonzero and descriptor counts representable",
            ));
        }
        Ok(Self {
            slots,
            max_bindings,
            cache: Mutex::new(DescriptorCache::default()),
        })
    }

    pub(super) fn is_prepared(&self, needs: ShaderNeeds) -> Result<bool, RuntimeError> {
        let cache = self
            .cache
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph descriptor cache lock poisoned"))?;
        Ok((!needs.compute || cache.layouts.0.is_some())
            && (!needs.composite || cache.layouts.1.is_some()))
    }

    pub(super) fn prepare(
        &self,
        owner: &Arc<MemoryOwner>,
        compute: Option<vk::DescriptorSetLayout>,
        composite: Option<vk::DescriptorSetLayout>,
        raster: [Option<RasterLayout>; RASTER_VARIANT_COUNT],
    ) -> Result<(), RuntimeError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph descriptor cache lock poisoned"))?;
        if cache.layouts == (compute, composite, raster) {
            return Ok(());
        }
        if cache.stats.descriptor_arenas_leased != 0 {
            return Err(RuntimeError::capacity_with(
                "Graph descriptor preparation requires retired leases",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Descriptors,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        let mut prepared = Vec::new();
        prepared.try_reserve_exact(self.slots).map_err(|_| {
            RuntimeError::capacity_with(
                "cannot reserve Graph descriptor arenas",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Descriptors,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            )
        })?;
        for ordinal in 0..self.slots {
            prepared.push(DescriptorArena::new(
                Arc::clone(owner),
                self.max_bindings,
                compute,
                composite,
                raster,
                cache.stats.descriptor_arenas_created + ordinal,
            )?);
        }
        cache.stats.descriptor_arenas_created += self.slots;
        cache.layouts = (compute, composite, raster);
        let previous = std::mem::replace(&mut cache.idle, prepared);
        drop(cache);
        drop(previous);
        Ok(())
    }

    pub(super) fn clear(&mut self) {
        self.cache
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .idle
            .clear();
    }
}

impl ShaderPrograms {
    pub(in crate::backends::vulkan) fn acquire_bindings(
        self: &Arc<Self>,
    ) -> Result<ShaderBindings, RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let mut cache = self
            .bindings
            .cache
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph descriptor cache lock poisoned"))?;
        if cache.stats.descriptor_arenas_leased == self.bindings.slots {
            return Err(RuntimeError::capacity_with(
                "Graph descriptor arena capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Descriptors,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        let arena = cache.idle.pop().ok_or_else(|| {
            RuntimeError::capacity_with(
                "Graph descriptor working set is not prepared or exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::Prepare,
                    None,
                    None,
                ),
            )
        })?;
        cache.stats.descriptor_arenas_reused += 1;
        cache.stats.descriptor_arenas_leased += 1;
        Ok(ShaderBindings {
            programs: Arc::clone(self),
            arena: Some(arena),
            used: 0,
            compute_used: 0,
            composite_used: 0,
            raster_used: [0; RASTER_VARIANT_COUNT],
        })
    }

    pub(in crate::backends::vulkan) fn stats(&self) -> ShaderStats {
        self.bindings
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stats
    }
}

/// Exclusive arena. Executor may release only for canceled/proven-unsubmitted
/// commands or after actual GPU retirement. Unknown completion must retain it.
pub(in crate::backends::vulkan) struct ShaderBindings {
    programs: Arc<ShaderPrograms>,
    arena: Option<DescriptorArena>,
    used: u32,
    compute_used: usize,
    composite_used: usize,
    raster_used: [usize; RASTER_VARIANT_COUNT],
}

impl ShaderBindings {
    fn prepared_set(&mut self, compute: bool) -> Result<vk::DescriptorSet, RuntimeError> {
        if self.used >= self.programs.bindings.max_bindings {
            return Err(RuntimeError::capacity_with(
                "Graph descriptor binding capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Descriptors,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            ));
        }
        let arena = self.arena.as_ref().expect("live descriptor lease");
        let (sets, used) = if compute {
            (&arena.compute, &mut self.compute_used)
        } else {
            (&arena.composite, &mut self.composite_used)
        };
        let set = *sets.get(*used).ok_or_else(|| {
            RuntimeError::capacity_with(
                "Graph descriptor layout is not prepared",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Descriptors,
                    CapacityRecovery::Prepare,
                    None,
                    None,
                ),
            )
        })?;
        *used += 1;
        self.used += 1;
        Ok(set)
    }

    pub(in crate::backends::vulkan) fn compute(
        &mut self,
        source: &DeviceImage,
        destination: &DeviceImage,
    ) -> Result<vk::DescriptorSet, RuntimeError> {
        if !source.belongs_to(&self.programs.owner)
            || !destination.belongs_to(&self.programs.owner)
            || source.format != vk::Format::R8G8B8A8_UNORM
            || destination.format != source.format
            || source.image == destination.image
            || source.extent != destination.extent
        {
            return Err(RuntimeError::invalid(
                "Compute bindings require distinct same-Device equal RGBA8 images",
            ));
        }
        if destination.extent.width.div_ceil(8)
            > self.programs.limits.max_compute_work_group_count[0]
            || destination.extent.height.div_ceil(8)
                > self.programs.limits.max_compute_work_group_count[1]
            || self.programs.limits.max_compute_work_group_count[2] < 1
        {
            return Err(RuntimeError::unsupported(
                "Compute dispatch exceeds adapter workgroup counts",
            ));
        }
        let set = self.prepared_set(true)?;
        let source_info = [vk::DescriptorImageInfo::default()
            .image_view(source.view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let destination_info = [vk::DescriptorImageInfo::default()
            .image_view(destination.view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&source_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&destination_info),
        ];
        // SAFETY: This exclusive lease updates each prepared set once. Prior
        // recorded uses canceled or retired and are never resubmitted; no
        // pending use remains. The executor retains matching image views.
        unsafe {
            self.programs
                .owner
                .device
                .raw()
                .update_descriptor_sets(&writes, &[])
        };
        Ok(set)
    }

    pub(in crate::backends::vulkan) fn raster(
        &mut self,
        key: usize,
        operands: impl Iterator<Item = (BindingLayout, RasterBinding)>,
    ) -> Result<[vk::DescriptorSet; RASTER_GROUP_COUNT], RuntimeError> {
        if self.used >= self.programs.bindings.max_bindings {
            return Err(RuntimeError::capacity_with(
                "Graph descriptor binding capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Descriptors,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            ));
        }
        let arena = self.arena.as_ref().expect("live descriptor lease");
        let sets = *arena.raster[key]
            .get(self.raster_used[key])
            .ok_or_else(|| {
                RuntimeError::capacity_with(
                    "Raster descriptor layout is not prepared",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Descriptors,
                        CapacityRecovery::Prepare,
                        None,
                        None,
                    ),
                )
            })?;
        self.raster_used[key] += 1;
        self.used += 1;
        let mut slots = [None; crate::program::MAX_RASTER_BINDINGS];
        let mut buffers =
            [vk::DescriptorBufferInfo::default(); crate::program::MAX_RASTER_BINDINGS];
        let mut images = [vk::DescriptorImageInfo::default(); crate::program::MAX_RASTER_BINDINGS];
        let mut count = 0;
        for (index, (slot, operand)) in operands.enumerate() {
            slots[index] = Some(slot);
            count += 1;
            match operand {
                RasterBinding::Buffer {
                    buffer,
                    offset,
                    size,
                } => {
                    let limits = &self.programs.limits;
                    let (alignment, maximum) = match slot.kind {
                        BindingType::Uniform => (
                            limits.min_uniform_buffer_offset_alignment,
                            limits.max_uniform_buffer_range,
                        ),
                        BindingType::StorageRead => (
                            limits.min_storage_buffer_offset_alignment,
                            limits.max_storage_buffer_range,
                        ),
                        _ => unreachable!("canonical Raster buffer binding"),
                    };
                    if !offset.is_multiple_of(alignment) || size > u64::from(maximum) {
                        return Err(RuntimeError::unsupported(
                            "Raster buffer alignment/range exceeds Device limits",
                        ));
                    }
                    buffers[index] = vk::DescriptorBufferInfo::default()
                        .buffer(buffer)
                        .offset(offset)
                        .range(size);
                }
                RasterBinding::Image(view) => {
                    images[index] = vk::DescriptorImageInfo::default()
                        .image_view(view)
                        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                }
                RasterBinding::Sampler(sampler) => {
                    images[index] = vk::DescriptorImageInfo::default().sampler(sampler);
                }
            }
        }
        let mut writes = [vk::WriteDescriptorSet::default(); crate::program::MAX_RASTER_BINDINGS];
        for (index, slot) in slots[..count].iter().enumerate() {
            let slot = slot.unwrap();
            let write = vk::WriteDescriptorSet::default()
                .dst_set(sets[slot.group as usize])
                .dst_binding(slot.binding)
                .descriptor_type(descriptor_type(slot.kind));
            writes[index] = match slot.kind {
                BindingType::Uniform | BindingType::StorageRead => {
                    write.buffer_info(std::slice::from_ref(&buffers[index]))
                }
                _ => write.image_info(std::slice::from_ref(&images[index])),
            };
        }
        // SAFETY: Canonical Graph operands established complete types/ranges.
        // The fixed arrays are fully initialized before their addresses are used.
        // This exclusive arena updates each set once before recording; GraphWork
        // pins native resources and the lease through cancellation or retirement.
        unsafe {
            self.programs
                .owner
                .device
                .raw()
                .update_descriptor_sets(&writes[..count], &[]);
        }
        Ok(sets)
    }

    pub(in crate::backends::vulkan) fn composite(
        &mut self,
        source: &DeviceImage,
    ) -> Result<vk::DescriptorSet, RuntimeError> {
        if !source.belongs_to(&self.programs.owner) || source.format != vk::Format::R8G8B8A8_UNORM {
            return Err(RuntimeError::invalid(
                "Composite binding requires same-Device RGBA8 image",
            ));
        }
        let set = self.prepared_set(false)?;
        let image_info = [vk::DescriptorImageInfo::default()
            .sampler(self.programs.composite().sampler)
            .image_view(source.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_info)];
        // SAFETY: The exclusive prepared set has no pending use and is immutable
        // after this lease's update. Old canceled/retired commands are never
        // resubmitted; executor retains the image/view through retirement.
        unsafe {
            self.programs
                .owner
                .device
                .raw()
                .update_descriptor_sets(&writes, &[])
        };
        Ok(set)
    }
}

impl Drop for ShaderBindings {
    fn drop(&mut self) {
        let Some(arena) = self.arena.take() else {
            return;
        };
        let mut cache = self
            .programs
            .bindings
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.stats.descriptor_arenas_leased -= 1;
        if self.programs.owner.device.ensure_active().is_err() {
            return;
        }
        // All sets remain allocated. Executor returns the lease only after
        // cancellation or retirement; old commands are never resubmitted.
        // A subsequent lease may update each set once before its next recording.
        cache.idle.push(arena);
    }
}

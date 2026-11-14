//! Descriptor arena ownership, bounded reuse and immutable binding leases.

#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::CString;
use std::sync::{Arc, Mutex};

use ash::vk;

use super::super::error::RuntimeError;
use super::super::memory::MemoryOwner;
use super::super::resources::DeviceImage;
#[cfg(feature = "vulkan-diagnostics")]
use super::name_object;
use super::{ShaderPrograms, native};

struct DescriptorArena {
    owner: Arc<MemoryOwner>,
    pool: vk::DescriptorPool,
}

impl DescriptorArena {
    fn new(
        owner: Arc<MemoryOwner>,
        max_bindings: u32,
        #[cfg_attr(not(feature = "vulkan-diagnostics"), allow(unused_variables))] ordinal: usize,
    ) -> Result<Self, RuntimeError> {
        let sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_IMAGE,
                descriptor_count: max_bindings * 2,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: max_bindings,
            },
        ];
        let info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(max_bindings)
            .pool_sizes(&sizes);
        // SAFETY: Counts checked at ShaderPrograms creation; this arena is
        // exclusively leased, and its pool remains live through recorded use.
        let pool = unsafe { owner.device.raw().create_descriptor_pool(&info, None) }
            .map_err(|result| native(&owner, "vkCreateDescriptorPool(Graph)", result))?;
        let result = Self { owner, pool };
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

#[derive(Default)]
struct DescriptorCache {
    idle: Vec<DescriptorArena>,
    stats: ShaderStats,
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
        if slots == 0 || max_bindings == 0 || max_bindings > u32::MAX / 2 {
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
            return Err(RuntimeError::capacity(
                "Graph descriptor arena capacity exhausted",
            ));
        }
        let arena = if let Some(arena) = cache.idle.pop() {
            cache.stats.descriptor_arenas_reused += 1;
            arena
        } else {
            let arena = DescriptorArena::new(
                Arc::clone(&self.owner),
                self.bindings.max_bindings,
                cache.stats.descriptor_arenas_created,
            )?;
            cache.stats.descriptor_arenas_created += 1;
            arena
        };
        cache.stats.descriptor_arenas_leased += 1;
        Ok(ShaderBindings {
            programs: Arc::clone(self),
            arena: Some(arena),
            used: 0,
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
}

impl ShaderBindings {
    fn allocate(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Result<vk::DescriptorSet, RuntimeError> {
        if self.used >= self.programs.bindings.max_bindings {
            return Err(RuntimeError::capacity(
                "Graph descriptor binding capacity exhausted",
            ));
        }
        let layouts = [layout];
        let info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.arena.as_ref().expect("live descriptor lease").pool)
            .set_layouts(&layouts);
        // SAFETY: This lease has exclusive host access to its pool. A freshly
        // allocated set has no command references and can be updated once.
        let sets = unsafe {
            self.programs
                .owner
                .device
                .raw()
                .allocate_descriptor_sets(&info)
        }
        .map_err(|r| native(&self.programs.owner, "vkAllocateDescriptorSets(Graph)", r))?;
        self.used += 1;
        Ok(sets[0])
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
        let set = self.allocate(self.programs.compute().set_layout)?;
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
        // SAFETY: Set is fresh/unbound and exclusively owned. Image views belong
        // to Device and match shader ABI; executor retains them until retirement.
        unsafe {
            self.programs
                .owner
                .device
                .raw()
                .update_descriptor_sets(&writes, &[])
        };
        Ok(set)
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
        let set = self.allocate(self.programs.composite().set_layout)?;
        let image_info = [vk::DescriptorImageInfo::default()
            .sampler(self.programs.composite().sampler)
            .image_view(source.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_info)];
        // SAFETY: Fresh set, immutable after this update, fixed combined sampler
        // ABI; executor retains image/view and this lease through retirement.
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
        // SAFETY: Executor releases this lease only after cancellation or proven
        // retirement. No pending command uses these sets; executable commands
        // invalidated by reset are never submitted again and reset before reuse.
        let reset = unsafe {
            self.programs
                .owner
                .device
                .raw()
                .reset_descriptor_pool(arena.pool, vk::DescriptorPoolResetFlags::empty())
        };
        match reset {
            Ok(()) => cache.idle.push(arena),
            Err(result) => {
                self.programs.owner.device.observe_result(result);
            }
        }
    }
}

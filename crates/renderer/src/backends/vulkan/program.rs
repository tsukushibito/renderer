//! Immutable native programs and their bounded binding allocator.
//!
//! Creation, descriptor ownership and command recording have separate modules.
//! Executors retain this owner and binding leases until cancellation or proven
//! retirement; publishing a new fixed program never replaces an existing one.

#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::CStr;
use std::sync::{Arc, Mutex, OnceLock};

use ash::vk;

use super::error::RuntimeError;
use super::memory::MemoryOwner;

mod bindings;
mod creation;
mod record;
pub(super) use record::attachment_info;

use bindings::BindingPool;
pub(super) use bindings::{RasterBinding, ShaderBindings};
use creation::{Program, ProgramKind};

const RASTER_VARIANT_COUNT: usize = crate::program::MAX_RASTER_PROGRAMS * 2;
const RASTER_GROUP_COUNT: usize = crate::program::MAX_RASTER_BIND_GROUPS;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RasterLayout {
    layouts: [vk::DescriptorSetLayout; RASTER_GROUP_COUNT],
    groups: usize,
    // Uniform, StorageRead, SampledImage, Sampler totals for this pipeline.
    descriptors: [u32; 4],
}

fn native(owner: &MemoryOwner, operation: &str, result: vk::Result) -> RuntimeError {
    owner.device.observe_result(result);
    RuntimeError::native(operation, owner.device.name(), result)
}

#[cfg(feature = "vulkan-diagnostics")]
fn name_object<H: vk::Handle>(
    owner: &MemoryOwner,
    handle: H,
    name: &CStr,
) -> Result<(), RuntimeError> {
    let info = vk::DebugUtilsObjectNameInfoEXT::default()
        .object_handle(handle)
        .object_name(name);
    // SAFETY: Every caller owns a live handle on this Device; the name remains
    // live through this immediate call and debug_utils is required at startup.
    unsafe {
        owner
            .device
            .debug_utils()
            .set_debug_utils_object_name(&info)
    }
    .map_err(|r| native(owner, "vkSetDebugUtilsObjectNameEXT(Graph)", r))
}

/// The live operations of one graph. Unused programs impose no capabilities or
/// native creation work; adding another kind never replaces a published program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ShaderNeeds {
    pub(super) compute: bool,
    pub(super) raster: bool,
    pub(super) composite: bool,
}

impl ShaderNeeds {
    pub(super) fn any(self) -> bool {
        self.compute || self.raster || self.composite
    }
}

/// Programs are published once by kind and retained for the runtime lifetime.
/// A short initialization lock allows adding a new kind while prior submissions
/// retain immutable programs; ordinary recording reads them without locking.
pub(super) struct ShaderPrograms {
    owner: Arc<MemoryOwner>,
    compute: OnceLock<Program>,
    raster: OnceLock<Program>,
    rasters: [OnceLock<Program>; RASTER_VARIANT_COUNT],
    composite: OnceLock<Program>,
    initialization: Mutex<()>,
    limits: vk::PhysicalDeviceLimits,
    bindings: BindingPool,
    implicit_raster: bool,
}

impl ShaderPrograms {
    pub(super) fn new(
        owner: Arc<MemoryOwner>,
        concurrent_slots: usize,
        max_bindings: u32,
        implicit_raster: bool,
    ) -> Result<Arc<Self>, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let bindings = BindingPool::new(concurrent_slots, max_bindings)?;
        let limits = *owner.device.limits();
        Ok(Arc::new(Self {
            owner,
            compute: OnceLock::new(),
            raster: OnceLock::new(),
            rasters: std::array::from_fn(|_| OnceLock::new()),
            composite: OnceLock::new(),
            initialization: Mutex::new(()),
            limits,
            bindings,
            implicit_raster,
        }))
    }

    pub(super) fn prepared_needs(&self) -> ShaderNeeds {
        ShaderNeeds {
            compute: self.compute.get().is_some(),
            raster: self.raster.get().is_some(),
            composite: self.composite.get().is_some(),
        }
    }

    /// Check the immutable prepared working set without creating native state.
    pub(super) fn ensure(&self, needs: ShaderNeeds) -> Result<(), RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if (!needs.compute || self.compute.get().is_some())
            && (!needs.composite || self.composite.get().is_some())
            && self.bindings.is_prepared(needs)?
        {
            Ok(())
        } else {
            Err(RuntimeError::capacity(
                "Graph program/binding working set is not prepared",
            ))
        }
    }

    /// Explicit preparation/change boundary. Existing leases must retire before
    /// extending the descriptor working set; ordinary ensure/acquire never grow.
    pub(super) fn prepare_fixed(&self, needs: ShaderNeeds) -> Result<(), RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let _initialization = self
            .initialization
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph program initialization lock poisoned"))?;
        for (needed, slot, kind) in [
            (needs.compute, &self.compute, ProgramKind::Compute),
            (
                needs.raster && self.implicit_raster,
                &self.raster,
                ProgramKind::Raster,
            ),
            (needs.composite, &self.composite, ProgramKind::Composite),
        ] {
            if needed && slot.get().is_none() {
                // Cache only successful creation; a transient OOM can be retried.
                let program = Program::new(Arc::clone(&self.owner), kind, &self.limits)?;
                slot.set(program).map_err(|_| {
                    RuntimeError::invalid(
                        "Graph program was unexpectedly published during initialization",
                    )
                })?;
            }
        }
        Ok(())
    }

    pub(super) fn prepare_bindings(&self, needs: ShaderNeeds) -> Result<(), RuntimeError> {
        self.prepare_fixed(needs)?;
        self.bindings.prepare(
            &self.owner,
            self.compute.get().map(|program| program.set_layout),
            self.composite.get().map(|program| program.set_layout),
            self.raster_layouts(),
        )
    }

    fn raster_layouts(&self) -> [Option<RasterLayout>; RASTER_VARIANT_COUNT] {
        std::array::from_fn(|key| {
            self.rasters[key]
                .get()
                .filter(|program| program.raster_groups != 0)
                .map(|program| {
                    let mut descriptors = [0; 4];
                    for slot in program.raster_asset.unwrap().bindings() {
                        descriptors[match slot.kind {
                            crate::program::BindingType::Uniform => 0,
                            crate::program::BindingType::StorageRead => 1,
                            crate::program::BindingType::SampledImage => 2,
                            crate::program::BindingType::Sampler => 3,
                            _ => unreachable!("trusted Raster read-only layout"),
                        }] += 1;
                    }
                    RasterLayout {
                        layouts: program.raster_layouts,
                        groups: program.raster_groups,
                        descriptors,
                    }
                })
        })
    }

    pub(in crate::backends::vulkan) fn raster_key(
        &self,
        managed: crate::program::ManagedRasterProgram,
        depth: bool,
    ) -> Option<usize> {
        self.rasters.iter().position(|slot| {
            slot.get().is_some_and(|program| {
                program
                    .raster_asset
                    .is_some_and(|asset| asset.same(&managed))
                    && program.raster_depth == depth
            })
        })
    }

    pub(super) fn prepare_rasters(
        &self,
        programs: &[crate::program::ManagedRasterProgram],
        depth_available: bool,
    ) -> Result<(), RuntimeError> {
        let distinct = programs
            .iter()
            .enumerate()
            .filter(|(index, program)| {
                !programs[..*index]
                    .iter()
                    .any(|earlier| earlier.same(program))
            })
            .count();
        if distinct > crate::program::MAX_RASTER_PROGRAMS {
            return Err(RuntimeError::capacity(
                "Raster Program working set exceeds the common capacity",
            ));
        }
        let _initialization = self
            .initialization
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph program initialization lock poisoned"))?;
        for &managed in programs {
            for depth in [false, true] {
                if (managed.requires_depth() && !depth)
                    || (depth && !depth_available && !managed.is_depth_only())
                {
                    continue;
                }
                if managed.same(&crate::program::ManagedRasterProgram::mesh())
                    && depth
                    && self.raster.get().is_some()
                {
                    continue;
                }
                if self.raster_key(managed, depth).is_none() {
                    let slot = self
                        .rasters
                        .iter()
                        .find(|slot| slot.get().is_none())
                        .ok_or_else(|| {
                            RuntimeError::capacity("Raster prepared Program capacity exhausted")
                        })?;
                    let program = Program::new_raster(Arc::clone(&self.owner), managed, depth)?;
                    slot.set(program)
                        .map_err(|_| RuntimeError::invalid("Raster unexpectedly published"))?;
                }
            }
        }
        self.bindings.prepare(
            &self.owner,
            self.compute.get().map(|program| program.set_layout),
            self.composite.get().map(|program| program.set_layout),
            self.raster_layouts(),
        )
    }

    pub(super) fn raster_program(
        &self,
        managed: crate::program::ManagedRasterProgram,
        depth: bool,
    ) -> Result<&Program, RuntimeError> {
        if managed.same(&crate::program::ManagedRasterProgram::mesh())
            && depth
            && let Some(program) = self.raster.get()
        {
            return Ok(program);
        }
        self.raster_key(managed, depth)
            .and_then(|key| self.rasters[key].get())
            .ok_or_else(|| RuntimeError::capacity("Raster Program variant is not prepared"))
    }

    fn compute(&self) -> &Program {
        self.compute
            .get()
            .expect("Compute program prepared before recording")
    }
    fn composite(&self) -> &Program {
        self.composite
            .get()
            .expect("Composite program prepared before recording")
    }

    #[cfg(test)]
    pub(super) fn handles(&self) -> [vk::Pipeline; 3] {
        [&self.compute, &self.raster, &self.composite].map(|slot| {
            slot.get()
                .map_or(vk::Pipeline::null(), |program| program.pipeline)
        })
    }
}

impl Drop for ShaderPrograms {
    fn drop(&mut self) {
        // Checked-out leases retain this owner. Destroy cached descriptor pools
        // before the separately owned immutable programs and layouts are dropped.
        self.bindings.clear();
    }
}

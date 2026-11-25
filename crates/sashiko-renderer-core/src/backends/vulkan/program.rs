//! Immutable native programs and their bounded binding allocator.
//!
//! Creation, descriptor ownership and command recording have separate modules.
//! Executors retain this owner and binding leases until cancellation or proven
//! retirement; implicit mesh and explicit Raster variants are immutable.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};
use crate::graph::SampleCount;
use crate::program::{ManagedRasterProgram, ProgramId, RasterPipelineRequest};

#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::CStr;
use std::sync::{Arc, Mutex, OnceLock};

use ash::vk;

use super::error::RuntimeError;
use super::memory::MemoryOwner;
use super::resources::sample_count_flags;

mod bindings;
mod creation;
mod record;
pub(super) use record::{
    NativeIndexedDraw, NativeRasterDraw, NativeRasterGeometry, RenderingAttachments,
    attachment_info,
};

use bindings::BindingPool;
pub(super) use bindings::{RasterBinding, ShaderBindings};
use creation::Program;

const RASTER_VARIANT_COUNT: usize = crate::program::MAX_RASTER_PROGRAMS * 2;
const RASTER_GROUP_COUNT: usize = crate::program::MAX_RASTER_BIND_GROUPS;

/// A prepared slot belongs to the retained immutable ShaderPrograms for one
/// Graph work item. Color/depth and sample compatibility are resolved before
/// selecting it.
#[derive(Clone, Copy)]
pub(super) enum RasterProgramSlot {
    FixedMesh,
    Managed(usize),
}

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

/// Programs are published once by kind and retained for the runtime lifetime.
/// A short initialization lock allows adding a new kind while prior submissions
/// retain immutable programs; ordinary recording reads them without locking.
pub(super) struct ShaderPrograms {
    owner: Arc<MemoryOwner>,
    raster: OnceLock<Program>,
    rasters: [OnceLock<Program>; RASTER_VARIANT_COUNT],
    initialization: Mutex<()>,
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
        Ok(Arc::new(Self {
            owner,
            raster: OnceLock::new(),
            rasters: std::array::from_fn(|_| OnceLock::new()),
            initialization: Mutex::new(()),
            bindings,
            implicit_raster,
        }))
    }

    /// Preserve the default depth-enabled mesh for callers preparing RGBA8 and D32.
    pub(super) fn prepare_implicit_raster(&self) -> Result<(), RuntimeError> {
        if !self.implicit_raster || self.raster.get().is_some() {
            return Ok(());
        }
        let _initialization = self
            .initialization
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph program initialization lock poisoned"))?;
        if self.raster.get().is_none() {
            let program = Program::new_raster(
                Arc::clone(&self.owner),
                &ManagedRasterProgram::mesh(),
                true,
                vk::SampleCountFlags::TYPE_1,
            )?;
            self.raster
                .set(program)
                .map_err(|_| RuntimeError::invalid("implicit Raster unexpectedly published"))?;
        }
        Ok(())
    }

    fn raster_layouts(&self) -> [Option<RasterLayout>; RASTER_VARIANT_COUNT] {
        std::array::from_fn(|key| {
            self.rasters[key]
                .get()
                .filter(|program| program.raster_groups != 0)
                .map(|program| {
                    let mut descriptors = [0; 4];
                    for slot in program.raster_asset.bindings() {
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

    fn raster_key(&self, id: ProgramId, depth: bool) -> Option<usize> {
        self.rasters.iter().position(|slot| {
            slot.get().is_some_and(|program| {
                program.raster_asset.identity() == id && program.raster_depth == depth
            })
        })
    }

    pub(super) fn prepare_rasters(
        &self,
        programs: &[impl Clone + Into<RasterPipelineRequest>],
        depth_samples: vk::SampleCountFlags,
    ) -> Result<(), RuntimeError> {
        let requests: Vec<_> = programs.iter().cloned().map(Into::into).collect();
        let distinct_programs = requests.iter().enumerate().filter(|(index, program)| {
            !requests[..*index]
                .iter()
                .any(|earlier| earlier.program.same(&program.program))
        });
        if distinct_programs.clone().count() > crate::program::MAX_RASTER_PROGRAMS {
            return Err(RuntimeError::capacity_with(
                "Raster Program working set exceeds the common capacity",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            ));
        }
        let _initialization = self
            .initialization
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph program initialization lock poisoned"))?;
        for (_, request) in distinct_programs {
            let managed = &request.program;
            let mut requested_samples = vk::SampleCountFlags::empty();
            for compatible in requests.iter().filter(|other| other.program.same(managed)) {
                requested_samples |= sample_count_flags(compatible.sample_count);
            }
            for depth in [false, true] {
                if managed.requires_depth() && !depth {
                    continue;
                }
                let mut samples = requested_samples;
                if depth && !managed.is_depth_only() {
                    samples &= depth_samples;
                }
                if depth
                    && managed.same(&ManagedRasterProgram::mesh())
                    && self.raster.get().is_some()
                {
                    samples &= !vk::SampleCountFlags::TYPE_1;
                }
                if samples.is_empty() {
                    continue;
                }
                let slot = self
                    .rasters
                    .iter()
                    .find(|slot| slot.get().is_none())
                    .ok_or_else(|| {
                        RuntimeError::capacity_with(
                            "Raster prepared Program capacity exhausted",
                            RuntimeCapacityError::new(
                                RuntimeCapacityKind::Programs,
                                CapacityRecovery::Reconfigure,
                                None,
                                None,
                            ),
                        )
                    })?;
                let program =
                    Program::new_raster(Arc::clone(&self.owner), managed, depth, samples)?;
                slot.set(program)
                    .map_err(|_| RuntimeError::invalid("Raster unexpectedly published"))?;
            }
        }
        self.bindings.prepare(&self.owner, self.raster_layouts())
    }

    pub(super) fn select_raster_program(
        &self,
        id: ProgramId,
        depth: bool,
        sample_count: SampleCount,
    ) -> Result<RasterProgramSlot, RuntimeError> {
        if sample_count == SampleCount::X1
            && id == ManagedRasterProgram::mesh().identity()
            && depth
            && self.raster.get().is_some()
        {
            return Ok(RasterProgramSlot::FixedMesh);
        }
        self.raster_key(id, depth)
            .filter(|&slot| {
                self.rasters[slot]
                    .get()
                    .is_some_and(|program| program.raster_pipeline(sample_count).is_some())
            })
            .map(RasterProgramSlot::Managed)
            .ok_or_else(|| {
                RuntimeError::capacity_with(
                    "Raster Program variant is not prepared",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Programs,
                        CapacityRecovery::Prepare,
                        None,
                        None,
                    ),
                )
            })
    }

    pub(super) fn raster_program(&self, slot: RasterProgramSlot) -> &Program {
        match slot {
            RasterProgramSlot::FixedMesh => self.raster.get(),
            RasterProgramSlot::Managed(slot) => self.rasters[slot].get(),
        }
        .expect("preflighted Raster Program slot")
    }
}

impl Drop for ShaderPrograms {
    fn drop(&mut self) {
        // Checked-out leases retain this owner. Destroy cached descriptor pools
        // before the separately owned immutable programs and layouts are dropped.
        self.bindings.clear();
    }
}

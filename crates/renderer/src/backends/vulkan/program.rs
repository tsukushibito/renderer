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

use bindings::BindingPool;
pub(super) use bindings::ShaderBindings;
use creation::{Program, ProgramKind};

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
    composite: OnceLock<Program>,
    initialization: Mutex<()>,
    limits: vk::PhysicalDeviceLimits,
    bindings: BindingPool,
}

impl ShaderPrograms {
    pub(super) fn new(
        owner: Arc<MemoryOwner>,
        concurrent_slots: usize,
        max_bindings: u32,
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
            composite: OnceLock::new(),
            initialization: Mutex::new(()),
            limits,
            bindings,
        }))
    }

    pub(super) fn ensure(&self, needs: ShaderNeeds) -> Result<(), RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if (!needs.compute || self.compute.get().is_some())
            && (!needs.raster || self.raster.get().is_some())
            && (!needs.composite || self.composite.get().is_some())
        {
            return Ok(());
        }
        let _initialization = self
            .initialization
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph program initialization lock poisoned"))?;
        for (needed, slot, kind) in [
            (needs.compute, &self.compute, ProgramKind::Compute),
            (needs.raster, &self.raster, ProgramKind::Raster),
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

    fn compute(&self) -> &Program {
        self.compute
            .get()
            .expect("Compute program prepared before recording")
    }
    fn raster(&self) -> &Program {
        self.raster
            .get()
            .expect("Raster program prepared before recording")
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

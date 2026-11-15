//! Independent prepared command/planning and CPU-result backings.

use ash::vk;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{PreparedPass, Use};
use crate::backends::vulkan::graph::{
    FrameCapacity, PhysicalLease, ResourceKey, RuntimeError, Snapshot,
    provenance::BufferProvenance, storage::reserved,
};
use crate::backends::vulkan::transfer::staging::StagingRange;

pub(in crate::backends::vulkan::graph) struct WorkData {
    pub(in crate::backends::vulkan::graph) physical: Vec<Option<PhysicalLease>>,
    pub(in crate::backends::vulkan::graph) keys: Vec<Option<ResourceKey>>,
    pub(in crate::backends::vulkan::graph) needed: Vec<bool>,
    pub(in crate::backends::vulkan::graph) final_states: Vec<Option<Snapshot>>,
    pub(super) passes: Vec<PreparedPass>,
    #[cfg(feature = "vulkan-diagnostics")]
    pub(super) labels: Vec<u8>,
    pub(super) buffers: Vec<vk::BufferMemoryBarrier2<'static>>,
    pub(super) images: Vec<vk::ImageMemoryBarrier2<'static>>,
    pub(super) uploads: Vec<StagingRange>,
    pub(super) uses: Vec<Use>,
    pub(super) touched: Vec<usize>,
    pub(super) used: Vec<bool>,
    pub(super) provenance_needed: Vec<bool>,
    pub(super) retain_provenance: Vec<bool>,
    pub(super) copy_scratch: BufferProvenance,
    pub(super) limit: FrameCapacity,
}
impl WorkData {
    pub(in crate::backends::vulkan::graph) fn new(
        limit: FrameCapacity,
    ) -> Result<Self, RuntimeError> {
        let resources = limit.graph.resources;
        let mut physical = reserved(resources)?;
        physical.resize_with(resources, || None);
        let mut keys = reserved(resources)?;
        keys.resize(resources, None);
        let mut needed = reserved(resources)?;
        needed.resize(resources, false);
        let mut uses = reserved(resources)?;
        uses.resize(resources, Use::default());
        let mut used = reserved(resources)?;
        used.resize(resources, false);
        let mut provenance_needed = reserved(resources)?;
        provenance_needed.resize(resources, false);
        let mut retain_provenance = reserved(resources)?;
        retain_provenance.resize(resources, false);
        let mut final_states = reserved(resources)?;
        for _ in 0..resources {
            final_states.push(Some(Snapshot::with_capacity(
                limit.initialized_ranges_per_resource,
                limit.provenance_segments_per_resource,
            )?));
        }
        Ok(Self {
            physical,
            keys,
            needed,
            final_states,
            uses,
            used,
            provenance_needed,
            retain_provenance,
            touched: reserved(resources)?,
            #[cfg(feature = "vulkan-diagnostics")]
            labels: reserved(
                limit
                    .graph
                    .name_bytes
                    .checked_add(limit.graph.passes)
                    .ok_or_else(|| RuntimeError::capacity("debug label capacity overflow"))?,
            )?,
            passes: reserved(limit.graph.passes)?,
            buffers: reserved(limit.graph.accesses)?,
            images: reserved(limit.graph.accesses)?,
            uploads: reserved(limit.graph.passes)?,
            copy_scratch: BufferProvenance::with_capacity(limit.provenance_segments_per_resource),
            limit,
        })
    }

    pub(in crate::backends::vulkan::graph) fn reset(&mut self) {
        for physical in &mut self.physical {
            physical.take();
        }
        self.keys.fill(None);
        self.needed.fill(false);
        self.used.fill(false);
        self.provenance_needed.fill(false);
        self.retain_provenance.fill(false);
        for state in self.final_states.iter_mut().flatten() {
            state.reset();
        }
        #[cfg(feature = "vulkan-diagnostics")]
        self.labels.clear();
        self.passes.clear();
        self.buffers.clear();
        self.images.clear();
        self.uploads.clear();
        self.touched.clear();
        self.uses.fill(Use::default());
        self.copy_scratch.clear();
    }
}

pub(in crate::backends::vulkan::graph) struct ResultData {
    pub(in crate::backends::vulkan::graph) cpu_pending: Arc<Mutex<[Duration; 2]>>,
    pub(in crate::backends::vulkan::graph) readbacks: Vec<(usize, Option<StagingRange>)>,
}
impl ResultData {
    pub(in crate::backends::vulkan::graph) fn new(passes: usize) -> Result<Self, RuntimeError> {
        Ok(Self {
            readbacks: reserved(passes)?,
            cpu_pending: Arc::new(Mutex::new([Duration::ZERO; 2])),
        })
    }
    pub(in crate::backends::vulkan::graph) fn reset(&mut self) {
        self.readbacks.clear();
        *self
            .cpu_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = [Duration::ZERO; 2];
    }
}

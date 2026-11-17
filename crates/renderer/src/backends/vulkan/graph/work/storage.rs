//! Independent prepared command/planning and CPU-result backings.

#[cfg(feature = "vulkan-diagnostics")]
use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

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
    // All resource-indexed arrays are prepared once. Only this prefix may be
    // read or modified by the current checkout, including partial preparation.
    pub(in crate::backends::vulkan::graph) active_resources: usize,
    pub(in crate::backends::vulkan::graph) physical: Vec<Option<PhysicalLease>>,
    pub(in crate::backends::vulkan::graph) keys: Vec<Option<ResourceKey>>,
    pub(in crate::backends::vulkan::graph) needed: Vec<bool>,
    pub(in crate::backends::vulkan::graph) final_states: Vec<Option<Snapshot>>,
    pub(super) passes: Vec<PreparedPass>,
    pub(super) raster_descriptors: Vec<[vk::DescriptorSet; crate::program::MAX_RASTER_BIND_GROUPS]>,
    pub(super) validated_raster_bindings: Vec<bool>,
    pub(super) active_raster_bindings: usize,
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
                limit.index_prefixes_per_resource,
            )?));
        }
        let mut validated_raster_bindings = reserved(limit.graph.bindings)?;
        validated_raster_bindings.resize(limit.graph.bindings, false);
        Ok(Self {
            active_resources: 0,
            validated_raster_bindings,
            active_raster_bindings: 0,
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
                    .ok_or_else(|| {
                        RuntimeError::capacity_with(
                            "debug label capacity overflow",
                            RuntimeCapacityError::new(
                                RuntimeCapacityKind::DiagnosticStorage,
                                CapacityRecovery::Reconfigure,
                                None,
                                None,
                            ),
                        )
                    })?,
            )?,
            passes: reserved(limit.graph.passes)?,
            raster_descriptors: reserved(limit.graph.draws)?,
            buffers: reserved(limit.graph.accesses)?,
            images: reserved(limit.graph.accesses)?,
            uploads: reserved(limit.graph.passes)?,
            copy_scratch: BufferProvenance::with_capacity(
                limit.provenance_segments_per_resource,
                limit.index_prefixes_per_resource,
            ),
            limit,
        })
    }

    pub(in crate::backends::vulkan::graph) fn reset(&mut self) {
        let resources = self.active_resources;
        for physical in &mut self.physical[..resources] {
            physical.take();
        }
        self.keys[..resources].fill(None);
        self.needed[..resources].fill(false);
        self.used[..resources].fill(false);
        self.provenance_needed[..resources].fill(false);
        self.retain_provenance[..resources].fill(false);
        for state in self.final_states[..resources].iter_mut().flatten() {
            state.reset();
        }
        #[cfg(feature = "vulkan-diagnostics")]
        self.labels.clear();
        self.passes.clear();
        self.raster_descriptors.clear();
        self.validated_raster_bindings[..self.active_raster_bindings].fill(false);
        self.active_raster_bindings = 0;
        self.buffers.clear();
        self.images.clear();
        self.uploads.clear();
        self.touched.clear();
        self.uses[..resources].fill(Use::default());
        self.copy_scratch.clear();
        self.active_resources = 0;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::vulkan::graph::storage::StoragePool;
    use crate::graph::GraphCapacity;

    #[test]
    fn cancelled_work_returns_active_metadata_across_shrinking_and_growing_graphs() {
        let capacity = FrameCapacity {
            graph: GraphCapacity {
                resources: 8,
                ..GraphCapacity::default()
            },
            initialized_ranges_per_resource: 1,
            provenance_segments_per_resource: 1,
            index_prefixes_per_resource: 1,
            ..FrameCapacity::default()
        };
        let pool = StoragePool::new(1, || WorkData::new(capacity), WorkData::reset).unwrap();
        let bytes: Arc<[u8]> = Arc::from([1u8; 4]);
        let prepared = pool.acquire().unwrap();
        let backing = prepared.final_states.as_ptr();
        let initialized = prepared.final_states[0]
            .as_ref()
            .unwrap()
            .initialized
            .as_ptr();
        drop(prepared);
        for resources in [6, 2, 8, 0, 3] {
            let mut work = pool.acquire().unwrap();
            assert_eq!(work.active_resources, 0);
            assert_eq!(work.active_raster_bindings, 0);
            assert!(
                work.validated_raster_bindings
                    .iter()
                    .all(|&validated| !validated)
            );
            work.validated_raster_bindings[resources] = true;
            work.active_raster_bindings = resources + 1;
            assert_eq!(work.final_states.as_ptr(), backing);
            assert_eq!(
                work.final_states[0].as_ref().unwrap().initialized.as_ptr(),
                initialized
            );
            for state in work.final_states.iter().flatten() {
                assert!(state.initialized.is_empty());
                assert_eq!(state.provenance.test_evidence(), (0, 0));
                assert_eq!(state.serial, 0);
            }
            assert!(work.needed.iter().all(|&needed| !needed));
            assert!(work.provenance_needed.iter().all(|&needed| !needed));
            assert!(work.retain_provenance.iter().all(|&retained| !retained));
            work.active_resources = resources;
            for index in 0..resources {
                work.needed[index] = true;
                work.provenance_needed[index] = true;
                work.retain_provenance[index] = true;
                let state = work.final_states[index].as_mut().unwrap();
                state.serial = 7;
                state
                    .set_initialized(std::slice::from_ref(&(0..4)))
                    .unwrap();
                state.provenance.write_slice(0..4, &bytes, 0..4).unwrap();
            }
            // Cancellation and a failed partial prepare both return this same
            // storage lease before any native submission is accepted.
            drop(work);
            assert_eq!(Arc::strong_count(&bytes), 1);
        }
    }
}

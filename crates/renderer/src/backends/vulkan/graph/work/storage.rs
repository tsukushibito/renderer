//! Independent prepared command/planning and CPU-result backings.

#[cfg(feature = "vulkan-diagnostics")]
use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use ash::vk;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{PreparedPass, Use};
use crate::backends::vulkan::graph::{
    FrameCapacity, PhysicalLease, ResourceKey, RuntimeError, Snapshot,
    provenance::BufferProvenance, storage::reserved,
};
use crate::backends::vulkan::transfer::staging::StagingRange;

type RasterSets = [vk::DescriptorSet; crate::program::MAX_RASTER_BIND_GROUPS];

#[derive(Clone, Copy, Default)]
struct RasterBindingEntry {
    required: [bool; 2],
    descriptors: [Option<(usize, RasterSets)>; 2],
}

/// One Graph binding range belongs to one immutable Program. Its only native
/// layout variants are the color-only and depth-enabled pipelines. Both the
/// physical table and Program owner stay fixed throughout this work checkout.
pub(in crate::backends::vulkan::graph) struct RasterBindingCache {
    entries: Vec<RasterBindingEntry>,
    active: usize,
}

impl RasterBindingCache {
    fn new(bindings: usize) -> Result<Self, RuntimeError> {
        let mut entries = reserved(bindings)?;
        entries.resize(bindings, RasterBindingEntry::default());
        Ok(Self { entries, active: 0 })
    }

    /// Count native descriptor reservations once per range and depth variant.
    /// Empty binding ranges consume no descriptors and need no cache entry.
    pub(in crate::backends::vulkan::graph) fn require(
        &mut self,
        bindings: &Range<usize>,
        depth: bool,
    ) -> bool {
        if bindings.is_empty() {
            return false;
        }
        self.active = self.active.max(bindings.end);
        let required = &mut self.entries[bindings.start].required[usize::from(depth)];
        let first = !*required;
        *required = true;
        first
    }

    pub(super) fn resolve(
        &mut self,
        bindings: &Range<usize>,
        depth: bool,
        layout: usize,
        create: impl FnOnce(bool) -> Result<RasterSets, RuntimeError>,
    ) -> Result<RasterSets, RuntimeError> {
        let entry = &mut self.entries[bindings.start];
        let variant = usize::from(depth);
        if let Some((cached_layout, sets)) = entry.descriptors[variant]
            && cached_layout == layout
        {
            return Ok(sets);
        }
        // Sampled operand suitability is independent of the depth variant.
        // Publish it together with successful native descriptor preparation;
        // failures must leave the range unvalidated for a later checkout.
        let validate_sampled = entry.descriptors.iter().all(Option::is_none);
        let sets = create(validate_sampled)?;
        entry.descriptors[variant] = Some((layout, sets));
        self.active = self.active.max(bindings.end);
        Ok(sets)
    }

    fn clear(&mut self) {
        self.entries[..self.active].fill(RasterBindingEntry::default());
        self.active = 0;
    }
}

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
    pub(in crate::backends::vulkan::graph) raster_bindings: RasterBindingCache,
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
        Ok(Self {
            active_resources: 0,
            raster_bindings: RasterBindingCache::new(limit.graph.bindings)?,
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
        self.raster_bindings.clear();
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
    fn raster_bindings_reuse_native_results_per_layout_and_reset_after_failure_or_return() {
        use ash::vk::Handle;
        let mut cache = RasterBindingCache::new(8).unwrap();
        let range = 2..5;
        let first = [vk::DescriptorSet::from_raw(11); crate::program::MAX_RASTER_BIND_GROUPS];
        let second = [vk::DescriptorSet::from_raw(12); crate::program::MAX_RASTER_BIND_GROUPS];
        let backing = cache.entries.as_ptr();
        assert!(!cache.require(&(0..0), false));
        assert_eq!(cache.active, 0);
        assert!(cache.require(&range, false));
        assert!(!cache.require(&range, false));
        assert!(cache.require(&range, true));
        assert!(!cache.require(&range, true));
        assert!(cache.require(&(5..8), false));
        assert!(
            cache
                .resolve(&range, false, 7, |validate| {
                    assert!(validate);
                    Err(RuntimeError::unsupported("rejected Device binding"))
                })
                .is_err()
        );
        assert_eq!(
            cache
                .resolve(&range, false, 7, |validate| {
                    assert!(validate, "failed preparation cannot publish a guarantee");
                    Ok(first)
                })
                .unwrap(),
            first
        );
        for _ in 0..100 {
            assert_eq!(
                cache
                    .resolve(&range, false, 7, |_| {
                        panic!("shared immutable bindings must not repeat native preparation")
                    })
                    .unwrap(),
                first
            );
        }
        assert_eq!(
            cache
                .resolve(&range, true, 8, |validate| {
                    assert!(!validate, "sampled suitability is shared by depth variants");
                    Ok(second)
                })
                .unwrap(),
            second
        );
        cache.clear();
        assert_eq!(cache.entries.as_ptr(), backing);
        assert_eq!(cache.active, 0);
        assert!(cache.require(&range, false));
        assert_eq!(
            cache
                .resolve(&range, false, 7, |validate| {
                    assert!(
                        validate,
                        "a new work must establish new physical guarantees"
                    );
                    Ok(second)
                })
                .unwrap(),
            second
        );
    }

    #[test]
    fn simultaneous_work_checkouts_do_not_share_raster_guarantees() {
        use ash::vk::Handle;
        let pool = StoragePool::new(
            2,
            || WorkData::new(FrameCapacity::default()),
            WorkData::reset,
        )
        .unwrap();
        let mut first = pool.acquire().unwrap();
        let mut second = pool.acquire().unwrap();
        for (work, handle) in [(&mut first, 1), (&mut second, 2)] {
            let sets =
                [vk::DescriptorSet::from_raw(handle); crate::program::MAX_RASTER_BIND_GROUPS];
            assert!(work.raster_bindings.require(&(0..1), false));
            assert_eq!(
                work.raster_bindings
                    .resolve(&(0..1), false, 0, |validate| {
                        assert!(validate);
                        Ok(sets)
                    })
                    .unwrap(),
                sets
            );
        }
    }

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
            assert_eq!(work.raster_bindings.active, 0);
            assert!(
                work.raster_bindings
                    .require(&(resources..resources + 1), false)
            );
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

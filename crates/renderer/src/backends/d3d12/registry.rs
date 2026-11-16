//! Canonical contents and prepared physical storage, independent of frame slots.
//! Every accepted recording ends in COMMON. Queue ordering therefore permits
//! pending producers/consumers without inventing CPU completion or state copies.

use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

use super::{
    D3D12Runtime, D3D12RuntimeError as Error, D3D12RuntimeErrorKind,
    bindings::*,
    frame::{D3D12FrameCapacity, ExecutionSlot},
    resources::{DeviceBuffer, DeviceImage},
};
use crate::graph::{
    BufferDesc, BufferHandle, CompiledGraph, ImageHandle, ImportBuffer, ImportImage,
    ImportRangesPool, ResourceDesc, ResourceKey, provenance::BufferProvenance,
};

enum Native {
    Buffer(DeviceBuffer),
    Image(DeviceImage),
}
struct Physical(Native);
// SAFETY: Only DEFAULT resources enter this store, never mapped host buffers.
// Native objects are free-threaded; queue access is serialized by queue_gate,
// and each recorder's mutable descriptor/state storage is exclusively leased.
unsafe impl Send for Physical {}
// SAFETY: Shared references expose only immutable native identity. Contents are
// accessed by ordered GPU commands, never by CPU mapping through this wrapper.
unsafe impl Sync for Physical {}

#[derive(Default)]
pub(super) struct PhysicalRef(Option<Arc<Physical>>);
impl PhysicalRef {
    pub(super) fn buffer(value: DeviceBuffer) -> Self {
        Self(Some(Arc::new(Physical(Native::Buffer(value)))))
    }
    pub(super) fn image(value: DeviceImage) -> Self {
        Self(Some(Arc::new(Physical(Native::Image(value)))))
    }
    pub(super) fn raw(&self) -> &ID3D12Resource {
        match &self.0.as_ref().expect("leased physical resource").0 {
            Native::Buffer(value) => value.raw(),
            Native::Image(value) => value.raw(),
        }
    }
    fn clear(&mut self) {
        self.0 = None;
    }
}

struct Entry {
    physical: Arc<Physical>,
    desc: ResourceDesc,
    generation: u32,
    registered: bool,
    revision: u64,
    initialized: Vec<Range<u64>>,
    provenance: BufferProvenance,
}
impl Entry {
    fn new(physical: Arc<Physical>, desc: ResourceDesc, capacity: D3D12FrameCapacity) -> Self {
        Self {
            physical,
            desc,
            generation: 0,
            registered: false,
            revision: 0,
            initialized: Vec::with_capacity(capacity.initialized_ranges_per_resource),
            provenance: BufferProvenance::with_capacity(capacity.provenance_segments_per_resource),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ResourceUse {
    key: ResourceKey,
    revision: u64,
    changed: bool,
}

#[derive(Clone)]
enum Export {
    Buffer(ImportBuffer),
    Image(ImportImage),
}
struct ExportSlot(Mutex<Vec<(usize, Export)>>);
pub(super) struct ExportLease {
    slot: Arc<ExportSlot>,
    graph: u64,
}
impl ExportLease {
    pub(super) fn buffer(&self, handle: BufferHandle) -> Result<ImportBuffer, Error> {
        if handle.graph != self.graph {
            return Err(Error::wrong_owner("foreign export Graph"));
        }
        let values = self.slot.0.lock().unwrap_or_else(|e| e.into_inner());
        match values.iter().find(|(index, _)| *index == handle.index) {
            Some((_, Export::Buffer(value))) => Ok(value.clone()),
            _ => Err(Error::invalid("buffer was not exported")),
        }
    }
    pub(super) fn image(&self, handle: ImageHandle) -> Result<ImportImage, Error> {
        if handle.graph != self.graph {
            return Err(Error::wrong_owner("foreign export Graph"));
        }
        let values = self.slot.0.lock().unwrap_or_else(|e| e.into_inner());
        match values.iter().find(|(index, _)| *index == handle.index) {
            Some((_, Export::Image(value))) => Ok(value.clone()),
            _ => Err(Error::invalid("image was not exported")),
        }
    }
}
impl Drop for ExportLease {
    fn drop(&mut self) {
        // Only this non-Clone lease and the registry own the slot. No references
        // to entries escape its mutex; cloned snapshots own independent tokens.
        self.slot
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

pub(super) struct Registry {
    owner: u64,
    entries: Vec<Entry>,
    ranges: ImportRangesPool,
    exports: Vec<Arc<ExportSlot>>,
    capacity: D3D12FrameCapacity,
}
fn stale() -> Error {
    Error::classified(
        D3D12RuntimeErrorKind::StaleResource,
        "stale resource; refresh and rebuild Graph",
    )
}
impl Registry {
    pub(super) fn new(capacity: D3D12FrameCapacity) -> Result<Self, Error> {
        if capacity.resource_capacity == 0 || capacity.resource_capacity > u32::MAX as usize {
            return Err(Error::invalid(
                "physical registry capacity must fit a nonzero u32",
            ));
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity.resource_capacity)
            .map_err(|_| Error::capacity("registry preparation failed"))?;
        let export_count = capacity
            .executions
            .checked_add(capacity.results)
            .ok_or_else(|| Error::capacity("export metadata capacity overflow"))?;
        let mut exports = Vec::with_capacity(export_count);
        for _ in 0..export_count {
            exports.push(Arc::new(ExportSlot(Mutex::new(Vec::with_capacity(
                capacity.resources,
            )))));
        }
        Ok(Self {
            owner: crate::graph::next_id(),
            entries,
            ranges: ImportRangesPool::new(
                capacity.initialized_ranges_per_resource,
                capacity.range_snapshots,
            )
            .map_err(|_| Error::capacity("import snapshot preparation failed"))?,
            exports,
            capacity,
        })
    }
    pub(super) fn install(&mut self, slot: &mut ExecutionSlot) -> Result<(), Error> {
        let images = slot.raster.as_ref().map_or(0, |r| {
            r.images
                .iter()
                .filter(|image| image.native.0.is_some())
                .count()
        });
        let additional = slot.buffers.iter().filter(|b| b.0.is_some()).count() + images;
        if self
            .entries
            .len()
            .checked_add(additional)
            .is_none_or(|n| n > self.capacity.resource_capacity)
        {
            return Err(Error::capacity("registry identity capacity exhausted"));
        }
        for buffer in &mut slot.buffers {
            if let Some(native) = buffer.0.take() {
                self.entries.push(Entry::new(
                    native,
                    ResourceDesc::Buffer(BufferDesc {
                        size: self.capacity.buffer_bytes,
                    }),
                    self.capacity,
                ));
            }
        }
        if let Some(storage) = &mut slot.raster {
            for image in &mut storage.images {
                if let Some(native) = image.native.0.take() {
                    self.entries.push(Entry::new(
                        native,
                        ResourceDesc::Image(image.desc),
                        self.capacity,
                    ));
                }
            }
        }
        Ok(())
    }
    /// Change-boundary guarantee, including synchronization with the last
    /// external token return. No new token can be made after exclusive success.
    pub(super) fn metadata_is_idle(&mut self) -> bool {
        self.exports
            .iter_mut()
            .all(|slot| Arc::get_mut(slot).is_some())
            && self.ranges.is_idle()
    }
    pub(super) fn registered_images(&self) -> Result<Vec<crate::graph::ImageDesc>, Error> {
        let mut images = Vec::new();
        images
            .try_reserve_exact(self.entries.iter().filter(|entry| entry.registered).count())
            .map_err(|_| Error::capacity("registered image preparation failed"))?;
        for entry in &self.entries {
            if entry.registered
                && let ResourceDesc::Image(desc) = entry.desc
            {
                images.push(desc);
            }
        }
        Ok(images)
    }
    fn reusable(entry: &Entry) -> bool {
        !entry.registered && entry.generation < u32::MAX && Arc::strong_count(&entry.physical) == 1
    }
    /// All allocation/native creation has completed in `prepared`. Check the
    /// index budget before moving anything; the commit below cannot fail.
    pub(super) fn replace_prepared(&mut self, prepared: &mut Self) -> Result<(), Error> {
        let reusable = self
            .entries
            .iter()
            .filter(|entry| Self::reusable(entry))
            .count();
        let additional = prepared.entries.len().saturating_sub(reusable);
        if additional > self.capacity.resource_capacity - self.entries.len() {
            return Err(Error::capacity(
                "registered and prepared resources exceed registry capacity",
            ));
        }
        let mut incoming = std::mem::take(&mut prepared.entries).into_iter();
        for entry in &mut self.entries {
            if Self::reusable(entry)
                && let Some(mut next) = incoming.next()
            {
                // An unregistered index becomes externally visible only when
                // bind advances this generation. Old deleted keys stay stale.
                next.generation = entry.generation;
                *entry = next;
            }
        }
        self.entries.extend(incoming);
        Ok(())
    }
    fn entry(&self, key: ResourceKey) -> Result<&Entry, Error> {
        if key.owner != self.owner {
            return Err(Error::wrong_owner("resource belongs to another runtime"));
        }
        self.entries
            .get(key.index as usize)
            .filter(|entry| entry.registered && entry.generation == key.generation)
            .ok_or_else(stale)
    }
    pub(super) fn refresh_buffer(&mut self, key: ResourceKey) -> Result<ImportBuffer, Error> {
        let entry = self.entry(key)?;
        let ResourceDesc::Buffer(desc) = entry.desc else {
            return Err(Error::invalid("resource is not a buffer"));
        };
        let revision = entry.revision;
        // Split fields after checking owner/generation under this same mutex.
        let initialized = self
            .ranges
            .capture(&self.entries[key.index as usize].initialized)
            .map_err(|_| Error::capacity("prepared import range snapshots exhausted"))?;
        Ok(ImportBuffer {
            key,
            revision,
            desc,
            initialized,
        })
    }
    pub(super) fn refresh_image(&self, key: ResourceKey) -> Result<ImportImage, Error> {
        let entry = self.entry(key)?;
        let ResourceDesc::Image(desc) = entry.desc else {
            return Err(Error::invalid("resource is not an image"));
        };
        Ok(ImportImage {
            key,
            revision: entry.revision,
            desc,
            initialized: !entry.initialized.is_empty(),
        })
    }
    pub(super) fn delete(&mut self, key: ResourceKey) -> Result<(), Error> {
        self.entry(key)?;
        let entry = &mut self.entries[key.index as usize];
        entry.registered = false;
        entry.initialized.clear();
        entry.provenance.clear();
        Ok(())
    }
    pub(super) fn bind(
        &mut self,
        graph: &CompiledGraph,
        slot: &mut ExecutionSlot,
    ) -> Result<(), Error> {
        // Check every declared import, including culled ones, at the mutable
        // external-state boundary. Immutable descriptors do not need rescanning.
        for (logical, resource) in graph.resources.iter().enumerate() {
            #[cfg(not(feature = "development-validation"))]
            let _ = logical;
            if let Some(key) = resource.import {
                let entry = self.entry(key)?;
                if entry.revision != resource.import_revision {
                    return Err(stale());
                }
                #[cfg(feature = "development-validation")]
                if entry.desc != resource.desc
                    || entry.initialized != graph.initial_contents(logical)
                {
                    return Err(stale());
                }
            }
        }
        slot.resource_uses[..graph.resources.len()].fill(None);
        slot.resource_count = graph.resources.len();
        for (logical, resource) in graph.resources.iter().enumerate() {
            let local = slot.resource_map[logical];
            if local == usize::MAX {
                continue;
            }
            if graph.final_contents(logical).len() > self.capacity.initialized_ranges_per_resource {
                return Err(Error::capacity(
                    "canonical initialized range capacity exhausted",
                ));
            }
            let index = if let Some(key) = resource.import {
                key.index as usize
            } else {
                let index = self
                    .entries
                    .iter()
                    .position(|entry| {
                        !entry.registered
                            && Arc::strong_count(&entry.physical) == 1
                            && entry.generation < u32::MAX
                            && match (entry.desc, resource.desc) {
                                (ResourceDesc::Buffer(_), ResourceDesc::Buffer(_)) => true,
                                (ResourceDesc::Image(a), ResourceDesc::Image(b)) => a == b,
                                _ => false,
                            }
                    })
                    .ok_or_else(|| {
                        Error::capacity("prepared physical resources are registered or leased")
                    })?;
                let entry = &mut self.entries[index];
                entry.generation += 1;
                entry.desc = resource.desc;
                entry.initialized.clear();
                entry.provenance.clear();
                index
            };
            let entry = &self.entries[index];
            let key = ResourceKey {
                owner: self.owner,
                index: index as u32,
                generation: entry.generation,
            };
            slot.resource_uses[logical] = Some(ResourceUse {
                key,
                revision: entry.revision,
                changed: false,
            });
            let native = PhysicalRef(Some(Arc::clone(&entry.physical)));
            match resource.desc {
                ResourceDesc::Buffer(_) => {
                    slot.buffers[local] = native;
                    slot.provenance[local]
                        .copy_from(&entry.provenance)
                        .map_err(|_| {
                            Error::capacity("canonical index provenance capacity exhausted")
                        })?;
                }
                ResourceDesc::Image(_) => {
                    slot.raster.as_mut().unwrap().images[local].native = native;
                }
            }
        }
        for &pass in &graph.order {
            for access in graph.accesses(pass) {
                if access.write || access.discard_after {
                    slot.resource_uses[access.resource]
                        .as_mut()
                        .unwrap()
                        .changed = true;
                }
            }
        }
        for value in slot.resource_uses[..slot.resource_count].iter().flatten() {
            if value.changed && value.revision == u64::MAX {
                return Err(Error::capacity("content revision exhausted"));
            }
        }
        Ok(())
    }
    pub(super) fn validate(&self, graph: &CompiledGraph) -> Result<(), Error> {
        for resource in &graph.resources {
            if let Some(key) = resource.import
                && self.entry(key)?.revision != resource.import_revision
            {
                return Err(stale());
            }
        }
        Ok(())
    }
    pub(super) fn capture_exports(
        &mut self,
        graph: &CompiledGraph,
        slot: &ExecutionSlot,
    ) -> Result<Option<ExportLease>, Error> {
        if graph.exports.is_empty() {
            return Ok(None);
        }
        let available = self
            .exports
            .iter_mut()
            .find_map(|s| {
                if Arc::get_mut(s).is_some() {
                    Some(s)
                } else {
                    None
                }
            })
            .ok_or_else(|| Error::capacity("prepared export metadata slots exhausted"))?;
        let lease = ExportLease {
            slot: Arc::clone(available),
            graph: graph.id,
        };
        {
            let mut values = lease.slot.0.lock().unwrap_or_else(|e| e.into_inner());
            values.clear();
            for &logical in &graph.exports {
                let usage = slot.resource_uses[logical].expect("export physical lease");
                let initialized = graph.final_contents(logical);
                let revision = usage.revision + u64::from(usage.changed);
                let export = match graph.resources[logical].desc {
                    ResourceDesc::Buffer(desc) => Export::Buffer(ImportBuffer {
                        key: usage.key,
                        revision,
                        desc,
                        initialized: self.ranges.capture(initialized).map_err(|_| {
                            Error::capacity("prepared export range snapshots exhausted")
                        })?,
                    }),
                    ResourceDesc::Image(desc) => Export::Image(ImportImage {
                        key: usage.key,
                        revision,
                        desc,
                        initialized: !initialized.is_empty(),
                    }),
                };
                values.push((logical, export));
            }
        }
        Ok(Some(lease))
    }
    /// All fallible capacity checks and export captures precede Execute/Signal.
    /// Publishing contents after Signal succeeds cannot allocate or fail.
    pub(super) fn commit(&mut self, graph: &CompiledGraph, slot: &mut ExecutionSlot) {
        for logical in 0..slot.resource_count {
            let Some(usage) = slot.resource_uses[logical] else {
                continue;
            };
            let entry = &mut self.entries[usage.key.index as usize];
            if usage.changed {
                entry.revision += 1;
            }
            entry.initialized.clear();
            entry
                .initialized
                .extend_from_slice(graph.final_contents(logical));
            if matches!(entry.desc, ResourceDesc::Buffer(_)) {
                std::mem::swap(
                    &mut entry.provenance,
                    &mut slot.provenance[slot.resource_map[logical]],
                );
            }
            if graph.has_export(logical) {
                entry.registered = true;
            }
            if !entry.registered {
                entry.provenance.clear();
            }
        }
    }
}

impl ExecutionSlot {
    pub(super) fn release_resources(&mut self) {
        for buffer in &mut self.buffers {
            buffer.clear();
        }
        if let Some(storage) = &mut self.raster {
            for image in &mut storage.images {
                image.native.clear();
            }
        }
        self.resource_uses[..self.resource_count].fill(None);
        self.resource_count = 0;
        for provenance in &mut self.provenance {
            provenance.clear();
        }
        self.provenance_scratch.clear();
    }
}

impl D3D12Runtime {
    /// Scheduled contents of accepted work; CPU completion is not implied.
    /// Return this buffer snapshot and all clones before changing the working set;
    /// its key and registered contents survive and can be refreshed afterwards.
    pub fn refresh_buffer(&self, key: ResourceKey) -> Result<ImportBuffer, Error> {
        self.device.ensure_active()?;
        self.pool
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .refresh_buffer(key)
    }
    /// Scheduled image contents, retained across working-set changes. This scalar
    /// snapshot has no pooled backing; CPU completion is observed separately.
    pub fn refresh_image(&self, key: ResourceKey) -> Result<ImportImage, Error> {
        self.device.ensure_active()?;
        self.pool
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .refresh_image(key)
    }
    /// Invalidates the key immediately. Recordings and pending GPU work retain
    /// physical leases until cancellation or actual Fence completion.
    pub fn delete(&mut self, key: ResourceKey) -> Result<(), Error> {
        self.pool
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .delete(key)
    }
}

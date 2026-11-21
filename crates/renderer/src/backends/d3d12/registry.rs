//! Canonical contents and prepared physical storage, independent of frame slots.
//! Every accepted recording ends in COMMON. Queue ordering therefore permits
//! pending producers/consumers without inventing CPU completion or state copies.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

use super::{
    D3D12Runtime, D3D12RuntimeError as Error, D3D12RuntimeErrorKind,
    bindings::*,
    frame::{D3D12BufferClass, D3D12FrameCapacity, ExecutionSlot},
    resources::{DeviceBuffer, DeviceImage},
};
use crate::graph::{
    BufferDesc, BufferHandle, CompiledGraph, ImageHandle, ImportBuffer, ImportImage,
    ImportRangesPool, ResourceDesc, ResourceKey, provenance::BufferProvenance,
};

mod reuse;
use reuse::{ReuseClass, ReuseIndex};
impl From<ResourceDesc> for ReuseClass {
    fn from(desc: ResourceDesc) -> Self {
        match desc {
            ResourceDesc::Buffer(desc) => Self::Buffer { size: desc.size },
            ResourceDesc::Image(desc) => Self::Image {
                width: desc.width,
                height: desc.height,
                format: desc.format as u8,
                mip_levels: desc.mip_levels,
                array_layers: desc.array_layers,
                sample_count: desc.sample_count.as_u32(),
            },
        }
    }
}

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

struct Entry<P = Physical> {
    // A retired free slot keeps its generation/index even after native storage
    // is released at an explicit preparation boundary.
    physical: Option<Arc<P>>,
    prepared: bool,
    desc: ResourceDesc,
    /// Native capacity remains immutable when a smaller logical resource uses
    /// this entry; changing logical contents must not shrink its reuse class.
    class: ReuseClass,
    generation: u32,
    registered: bool,
    revision: u64,
    initialized: Vec<Range<u64>>,
    provenance: BufferProvenance,
}
impl<P> Entry<P> {
    fn new(physical: Arc<P>, desc: ResourceDesc, capacity: D3D12FrameCapacity) -> Self {
        Self {
            physical: Some(physical),
            prepared: true,
            desc,
            class: desc.into(),
            generation: 0,
            registered: false,
            revision: 0,
            initialized: Vec::with_capacity(capacity.initialized_ranges_per_resource),
            provenance: BufferProvenance::with_capacity(
                capacity.provenance_segments_per_resource,
                capacity.index_prefixes_per_resource,
            ),
        }
    }
    fn fits(&self, desc: ResourceDesc) -> bool {
        if !self.prepared || self.physical.is_none() {
            return false;
        }
        match (self.class, desc) {
            (ReuseClass::Buffer { size }, ResourceDesc::Buffer(desc)) => size >= desc.size,
            (_, ResourceDesc::Image(_)) => self.class == desc.into(),
            _ => false,
        }
    }
    fn physical(&self) -> &Arc<P> {
        self.physical.as_ref().expect("live native resource")
    }
    fn replaceable(&self) -> bool {
        !self.registered
            && self.generation < u32::MAX
            && self
                .physical
                .as_ref()
                .is_none_or(|value| Arc::strong_count(value) == 1)
    }
    fn retire(&mut self) {
        self.prepared = false;
        self.initialized.clear();
        self.provenance.clear();
        // An escaped native consumer can retain a deleted resource. Keep this
        // anchor until a later explicit boundary observes sole ownership, so
        // its eventual lease return cannot perform the last native destruction
        // in an ordinary frame. Inactive entries never rejoin candidate lists.
        if self
            .physical
            .as_ref()
            .is_none_or(|value| Arc::strong_count(value) == 1)
        {
            self.physical = None;
        }
    }
}

fn replace_entries<P>(
    entries: &mut Vec<Entry<P>>,
    prepared: &mut Vec<Entry<P>>,
    capacity: usize,
) -> Result<(), Error> {
    let replaceable = entries.iter().filter(|entry| entry.replaceable()).count();
    let additional = prepared.len().saturating_sub(replaceable);
    if entries
        .len()
        .checked_add(additional)
        .is_none_or(|count| count > capacity)
    {
        return Err(Error::capacity_with(
            "registered and prepared resources exceed registry capacity",
            RuntimeCapacityError::new(
                RuntimeCapacityKind::Resources,
                CapacityRecovery::Reconfigure,
                None,
                None,
            ),
        ));
    }
    // All allocation/native creation and the whole index-budget check precede
    // mutation. Registered indices are never shifted or replaced.
    let mut incoming = std::mem::take(prepared).into_iter();
    for entry in entries.iter_mut().filter(|entry| !entry.registered) {
        if entry.replaceable()
            && let Some(mut next) = incoming.next()
        {
            // bind advances this generation before exposing a new resource.
            next.generation = entry.generation;
            *entry = next;
        } else {
            entry.retire();
        }
    }
    entries.extend(incoming);
    Ok(())
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
    reuse: ReuseIndex<ReuseClass>,
    ranges: ImportRangesPool,
    exports: Vec<Arc<ExportSlot>>,
    result_returns: Arc<()>,
    capacity: D3D12FrameCapacity,
}
fn stale() -> Error {
    Error::classified(
        D3D12RuntimeErrorKind::StaleResource,
        "stale resource; refresh and rebuild Graph",
    )
}
impl Registry {
    /// On-demand logical accounting; native requirements and transfer arenas
    /// are deliberately reported through their separate owners.
    pub(super) fn logical_memory_bytes(&self) -> [u64; 3] {
        let mut bytes = [0u64; 3];
        for entry in &self.entries {
            let Some(physical) = &entry.physical else {
                continue;
            };
            let size = match entry.desc {
                ResourceDesc::Buffer(desc) => desc.size,
                ResourceDesc::Image(desc) => desc.byte_len().unwrap_or(u64::MAX),
            };
            let kind = if entry.registered {
                0
            } else if Arc::strong_count(physical) > 1 {
                1
            } else {
                2
            };
            bytes[kind] = bytes[kind].saturating_add(size);
        }
        bytes
    }
    #[cfg(feature = "presentation")]
    pub(super) fn owner_id(&self) -> u64 {
        self.owner
    }
    /// Validate mutable import identity/revision under the queue transaction and
    /// retain its native source until presentation's sampling commands retire.
    /// Every Graph and presentation recording returns the resource to COMMON.
    #[cfg(feature = "presentation")]
    pub(super) fn presentation_source(&self, image: &ImportImage) -> Result<PhysicalRef, Error> {
        let entry = self.entry(image.key())?;
        if entry.revision != image.revision() {
            return Err(stale());
        }
        if entry.desc != ResourceDesc::Image(image.desc())
            || !fully_initialized(&entry.initialized, image.desc().mip_levels)
        {
            return Err(Error::invalid(
                "presentation source is not a fully initialized image",
            ));
        }
        Ok(PhysicalRef(Some(Arc::clone(entry.physical()))))
    }
    pub(super) fn new(capacity: D3D12FrameCapacity) -> Result<Self, Error> {
        if capacity.resource_capacity == 0 || capacity.resource_capacity > u32::MAX as usize {
            return Err(Error::invalid(
                "physical registry capacity must fit a nonzero u32",
            ));
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity.resource_capacity)
            .map_err(|_| {
                Error::capacity_with(
                    "registry preparation failed",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::HostMemory,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
        let export_count = capacity
            .executions
            .checked_add(capacity.results)
            .ok_or_else(|| {
                Error::capacity_with(
                    "export metadata capacity overflow",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::ExportMetadata,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
        let mut exports = Vec::with_capacity(export_count);
        for _ in 0..export_count {
            exports.push(Arc::new(ExportSlot(Mutex::new(Vec::with_capacity(
                capacity.resources,
            )))));
        }
        Ok(Self {
            owner: crate::graph::next_id(),
            entries,
            reuse: ReuseIndex::new(capacity.resource_capacity).map_err(|_| {
                Error::capacity_with(
                    "resource selection preparation failed",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::HostMemory,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?,
            ranges: ImportRangesPool::new(
                capacity.initialized_ranges_per_resource,
                capacity.range_snapshots,
            )
            .map_err(|_| {
                Error::capacity_with(
                    "import snapshot preparation failed",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::GraphMetadata,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?,
            exports,
            result_returns: Arc::new(()),
            capacity,
        })
    }
    pub(super) fn prepare_buffers(
        &mut self,
        owner: &super::device::DeviceOwner,
        classes: &[D3D12BufferClass],
    ) -> Result<(), Error> {
        // The complete class/count budget was checked before any native work.
        for class in classes {
            for _ in 0..class.count {
                let buffer = DeviceBuffer::storage(owner, class.size)?;
                self.entries.push(Entry::new(
                    Arc::new(Physical(Native::Buffer(buffer))),
                    ResourceDesc::Buffer(BufferDesc { size: class.size }),
                    self.capacity,
                ));
            }
        }
        self.rebuild_reuse_index();
        Ok(())
    }
    pub(super) fn install(&mut self, slot: &mut ExecutionSlot) -> Result<(), Error> {
        let images = slot.raster.as_ref().map_or(0, |r| {
            r.images
                .iter()
                .filter(|image| image.native.0.is_some())
                .count()
        });
        let additional = images;
        if self
            .entries
            .len()
            .checked_add(additional)
            .is_none_or(|n| n > self.capacity.resource_capacity)
        {
            return Err(Error::capacity_with(
                "registry identity capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Resources,
                    CapacityRecovery::Reconfigure,
                    Some(self.capacity.resource_capacity as u64),
                    self.entries
                        .len()
                        .checked_add(additional)
                        .map(|required| required as u64),
                ),
            ));
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
        self.rebuild_reuse_index();
        Ok(())
    }
    fn rebuild_reuse_index(&mut self) {
        self.reuse.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            if !entry.prepared || entry.physical.is_none() {
                continue;
            }
            self.reuse.install(index, entry.class);
            if entry.registered || entry.generation == u32::MAX {
                self.reuse.remove(index);
            }
        }
    }
    /// Separate from the native readback lease: even a fully consumed result is
    /// a public change-boundary token until Drop, matching Vulkan. This anchor
    /// moves with the registry, so ordinary result return never frees backing.
    pub(super) fn result_token(&self) -> Arc<()> {
        Arc::clone(&self.result_returns)
    }
    /// Change-boundary guarantee, including synchronization with the last
    /// external token return. No new token can be made after exclusive success.
    pub(super) fn metadata_is_idle(&mut self) -> bool {
        if Arc::get_mut(&mut self.result_returns).is_none() {
            return false;
        }
        self.exports
            .iter_mut()
            .all(|slot| Arc::get_mut(slot).is_some())
            && self.ranges.is_idle()
    }
    pub(super) fn registered_images(&self) -> Result<Vec<crate::graph::ImageDesc>, Error> {
        let mut images = Vec::new();
        images
            .try_reserve_exact(self.entries.iter().filter(|entry| entry.registered).count())
            .map_err(|_| {
                Error::capacity_with(
                    "registered image preparation failed",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Resources,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
        for entry in &self.entries {
            if entry.registered
                && let ResourceDesc::Image(desc) = entry.desc
            {
                images.push(desc);
            }
        }
        Ok(images)
    }
    /// All allocation/native creation has completed in `prepared`. Check the
    /// index budget before moving anything; the commit below cannot fail.
    pub(super) fn replace_prepared(&mut self, prepared: &mut Self) -> Result<(), Error> {
        replace_entries(
            &mut self.entries,
            &mut prepared.entries,
            self.capacity.resource_capacity,
        )?;
        self.rebuild_reuse_index();
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
    fn register_buffer(&mut self, desc: BufferDesc) -> Result<ResourceKey, Error> {
        if desc.size == 0 || desc.size > self.capacity.buffer_bytes {
            return Err(Error::invalid(
                "buffer size must fit the prepared buffer limit",
            ));
        }
        let desc = ResourceDesc::Buffer(desc);
        // Registration is an explicit preparation boundary. Pick the smallest
        // prepared buffer large enough for the new logical size.
        let index = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.fits(desc) && entry.replaceable())
            .min_by_key(|(_, entry)| entry.class)
            .map(|(index, _)| index)
            .ok_or_else(|| {
                let leased = self.entries.iter().any(|entry| {
                    !entry.registered && entry.generation < u32::MAX && entry.fits(desc)
                });
                Error::capacity_with(
                    "no available prepared buffer for registration",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Resources,
                        if leased {
                            CapacityRecovery::ReleaseOwners
                        } else {
                            CapacityRecovery::Reconfigure
                        },
                        None,
                        None,
                    ),
                )
            })?;
        let entry = &mut self.entries[index];
        entry.generation += 1;
        entry.registered = true;
        entry.desc = desc;
        entry.initialized.clear();
        entry.provenance.clear();
        self.reuse.remove(index);
        Ok(ResourceKey {
            owner: self.owner,
            index: index as u32,
            generation: entry.generation,
        })
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
            .map_err(|error| {
                Error::capacity_with(
                    "prepared import range snapshots exhausted",
                    crate::capacity::import_ranges_capacity(&error),
                )
            })?;
        ImportBuffer::new(key, revision, desc, initialized)
            .map_err(|error| Error::invalid(error.to_string()))
    }
    pub(super) fn refresh_image(&self, key: ResourceKey) -> Result<ImportImage, Error> {
        let entry = self.entry(key)?;
        let ResourceDesc::Image(desc) = entry.desc else {
            return Err(Error::invalid("resource is not an image"));
        };
        ImportImage::new(key, entry.revision, desc, &entry.initialized)
            .map_err(|error| Error::invalid(error.to_string()))
    }
    pub(super) fn delete(&mut self, key: ResourceKey) -> Result<(), Error> {
        self.entry(key)?;
        let entry = &mut self.entries[key.index as usize];
        entry.registered = false;
        entry.initialized.clear();
        entry.provenance.clear();
        if entry.generation < u32::MAX {
            self.reuse.restore(key.index as usize);
        }
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
        self.reuse.begin();
        for (logical, resource) in graph.resources.iter().enumerate() {
            let local = slot.resource_map[logical];
            if local == usize::MAX {
                continue;
            }
            if graph.final_contents(logical).len() > self.capacity.initialized_ranges_per_resource {
                return Err(Error::capacity_with(
                    "canonical initialized range capacity exhausted",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::GraphMetadata,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                ));
            }
            let index = if let Some(key) = resource.import {
                key.index as usize
            } else {
                let entries = &self.entries;
                let eligible = |index: usize| {
                    let entry = &entries[index];
                    !entry.registered
                        && entry
                            .physical
                            .as_ref()
                            .is_some_and(|value| Arc::strong_count(value) == 1)
                        && entry.generation < u32::MAX
                        && entry.fits(resource.desc)
                };
                let selected = match resource.desc {
                    ResourceDesc::Buffer(_) => self.reuse.select_from(
                        resource.desc.into(),
                        |class| matches!(class, ReuseClass::Buffer { .. }),
                        eligible,
                    ),
                    ResourceDesc::Image(_) => self.reuse.select(resource.desc.into(), eligible),
                };
                let index = selected.ok_or_else(|| {
                    // Diagnose only the failed request: instances retained
                    // by another frame can return, while instances already
                    // borrowed by this Graph cannot satisfy another operand.
                    let mut limit = 0u64;
                    let mut used = 0u64;
                    let mut retained_elsewhere = false;
                    for (index, entry) in self.entries.iter().enumerate() {
                        let matches = entry.fits(resource.desc);
                        if entry.registered || entry.generation == u32::MAX || !matches {
                            continue;
                        }
                        limit += 1;
                        if slot.resource_uses[..logical]
                            .iter()
                            .flatten()
                            .any(|use_| use_.key.index as usize == index)
                        {
                            used += 1;
                        } else if Arc::strong_count(entry.physical()) > 1 {
                            retained_elsewhere = true;
                        }
                    }
                    Error::capacity_with(
                        "prepared physical resources are registered or leased",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::Resources,
                            if retained_elsewhere {
                                CapacityRecovery::ReleaseOwners
                            } else {
                                CapacityRecovery::Reconfigure
                            },
                            Some(limit),
                            Some(used + 1),
                        ),
                    )
                })?;
                let entry = &mut self.entries[index];
                entry.generation += 1;
                if entry.generation == u32::MAX {
                    self.reuse.remove(index);
                }
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
            let native = PhysicalRef(Some(Arc::clone(entry.physical())));
            match resource.desc {
                ResourceDesc::Buffer(_) => {
                    slot.buffers[local] = native;
                    slot.provenance[local]
                        .copy_from(&entry.provenance)
                        .map_err(|_| {
                            Error::capacity_with(
                                "canonical index provenance capacity exhausted",
                                RuntimeCapacityError::new(
                                    RuntimeCapacityKind::GraphMetadata,
                                    CapacityRecovery::Reconfigure,
                                    None,
                                    None,
                                ),
                            )
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
                return Err(Error::capacity_with(
                    "content revision exhausted",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Identity,
                        CapacityRecovery::Recreate,
                        None,
                        None,
                    ),
                ));
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
        let export_slots = self.exports.len() as u64;
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
            .ok_or_else(|| {
                Error::capacity_with(
                    "prepared export metadata slots exhausted",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::ExportMetadata,
                        CapacityRecovery::ReleaseOwners,
                        Some(export_slots),
                        export_slots.checked_add(1),
                    ),
                )
            })?;
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
                    ResourceDesc::Buffer(desc) => Export::Buffer(
                        ImportBuffer::new(
                            usage.key,
                            revision,
                            desc,
                            self.ranges.capture(initialized).map_err(|error| {
                                Error::capacity_with(
                                    "prepared export range snapshots exhausted",
                                    crate::capacity::import_ranges_capacity(&error)
                                        .simultaneous_slots(
                                            graph
                                                .exports
                                                .iter()
                                                .filter(|&&resource| {
                                                    matches!(
                                                        graph.resources[resource].desc,
                                                        ResourceDesc::Buffer(_)
                                                    )
                                                })
                                                .count()
                                                as u64,
                                        ),
                                )
                            })?,
                        )
                        .map_err(|error| Error::invalid(error.to_string()))?,
                    ),
                    ResourceDesc::Image(desc) => Export::Image(
                        ImportImage::new(usage.key, revision, desc, initialized)
                            .map_err(|error| Error::invalid(error.to_string()))?,
                    ),
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
                self.reuse.remove(usage.key.index as usize);
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
    pub(crate) fn register_buffer(&mut self, desc: BufferDesc) -> Result<ResourceKey, Error> {
        self.device.ensure_active()?;
        self.pool
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .register_buffer(desc)
    }

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

#[cfg(any(test, feature = "presentation"))]
fn fully_initialized(ranges: &[Range<u64>], mip_levels: u32) -> bool {
    let mut end = 0;
    for range in ranges {
        if range.start > end {
            return false;
        }
        end = end.max(range.end);
        if end >= u64::from(mip_levels) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Substitute only the external native owner. Production Entry metadata,
    // Arc lease checks and the complete replacement algorithm are exercised.
    struct NativeDrop(Arc<AtomicUsize>);
    impl Drop for NativeDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn entry(desc: ResourceDesc, drops: &Arc<AtomicUsize>) -> Entry<NativeDrop> {
        Entry::new(
            Arc::new(NativeDrop(Arc::clone(drops))),
            desc,
            D3D12FrameCapacity::default(),
        )
    }
    fn buffer(size: u64) -> ResourceDesc {
        ResourceDesc::Buffer(BufferDesc { size })
    }

    #[test]
    fn preparation_shrink_retires_surplus_native_storage_and_preserves_registered_identity() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut entries = vec![
            entry(buffer(512), &drops),
            entry(buffer(64), &drops),
            entry(buffer(128), &drops),
        ];
        entries[0].generation = 7;
        entries[1].generation = 9;
        entries[2].generation = 11;
        entries[2].registered = true;
        entries[2].revision = 23;
        entries[2].initialized.push(0..128);
        let registered = Arc::as_ptr(entries[2].physical());
        let mut prepared = vec![entry(buffer(8), &drops)];
        replace_entries(&mut entries, &mut prepared, 3).unwrap();
        assert!(prepared.is_empty());
        assert!(entries[0].fits(buffer(8)));
        assert!(!entries[0].fits(buffer(64)));
        assert_eq!(entries[0].generation, 7);
        assert!(!entries[1].fits(buffer(8)));
        assert!(entries[1].physical.is_none());
        assert_eq!(entries[1].generation, 9);
        assert_eq!(Arc::as_ptr(entries[2].physical()), registered);
        assert_eq!((entries[2].generation, entries[2].revision), (11, 23));
        assert_eq!(
            entries[2].initialized.as_slice(),
            std::slice::from_ref(&(0..128))
        );
        assert_eq!(drops.load(Ordering::Relaxed), 2);

        replace_entries(&mut entries, &mut vec![], 3).unwrap();
        assert!(!entries[0].fits(buffer(1)));
        assert!(entries[0].physical.is_none());
        assert_eq!(drops.load(Ordering::Relaxed), 3);
        assert_eq!(Arc::as_ptr(entries[2].physical()), registered);
        let mut prepared = vec![
            entry(buffer(16), &drops),
            entry(
                ResourceDesc::Image(crate::graph::ImageDesc::rgba8(2, 2)),
                &drops,
            ),
        ];
        replace_entries(&mut entries, &mut prepared, 3).unwrap();
        assert_eq!((entries[0].generation, entries[1].generation), (7, 9));
        assert!(entries[0].fits(buffer(16)));
        assert!(entries[1].fits(ResourceDesc::Image(crate::graph::ImageDesc::rgba8(2, 2))));
        assert_eq!(Arc::as_ptr(entries[2].physical()), registered);
    }

    #[test]
    fn retired_lease_cannot_reenter_selection_or_release_its_last_anchor_in_a_frame() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut entries = vec![entry(buffer(512), &drops)];
        let escaped = Arc::clone(entries[0].physical());
        let mut prepared = vec![entry(buffer(8), &drops)];
        assert!(replace_entries(&mut entries, &mut prepared, 1).is_err());
        assert!(entries[0].prepared);
        assert_eq!(prepared.len(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        replace_entries(&mut entries, &mut vec![], 1).unwrap();
        assert!(!entries[0].fits(buffer(1)));
        assert!(entries[0].physical.is_some());
        drop(escaped);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        replace_entries(&mut entries, &mut vec![], 1).unwrap();
        assert!(entries[0].physical.is_none());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        replace_entries(&mut entries, &mut prepared, 1).unwrap();
        assert!(entries[0].fits(buffer(8)));
    }

    #[test]
    fn terminal_generation_can_retire_storage_but_cannot_recycle_its_identity() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut entries = vec![entry(buffer(8), &drops)];
        entries[0].generation = u32::MAX;
        replace_entries(&mut entries, &mut vec![], 1).unwrap();
        assert!(entries[0].physical.is_none());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        let mut prepared = vec![entry(buffer(8), &drops)];
        assert!(replace_entries(&mut entries, &mut prepared, 1).is_err());
        assert_eq!(entries[0].generation, u32::MAX);
        assert_eq!(prepared.len(), 1);
    }

    #[test]
    #[ignore = "requires real D3D12 hardware; root serializes native acceptance"]
    fn d3d12_working_set_shrink_removes_old_classes_and_preserves_registered_contents() {
        use crate::d3d12::{D3D12RuntimeConfig, D3D12ValidationMode};
        use crate::graph::{GraphCapacity, GraphPool};
        use std::time::Duration;

        let strict = cfg!(feature = "d3d12-diagnostics");
        let mut runtime = D3D12Runtime::new_with_working_set(
            D3D12RuntimeConfig {
                validation: if strict {
                    D3D12ValidationMode::Required
                } else {
                    D3D12ValidationMode::Disabled
                },
                capacity: D3D12FrameCapacity {
                    executions: 1,
                    results: 1,
                    resources: 8,
                    operations: 8,
                    buffers: 3,
                    buffer_bytes: 512,
                    resource_capacity: 8,
                    upload_bytes: 512,
                    readback_bytes: 512,
                    scratch_bytes: 4,
                    ..Default::default()
                },
            },
            &[D3D12BufferClass { size: 64, count: 3 }],
            &[],
            &[],
            &[],
        )
        .unwrap();
        let monitor = runtime.validation_monitor();
        let key = {
            let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
            let mut graph = pool.begin().unwrap();
            let buffer = graph
                .buffer("registered contents", BufferDesc { size: 16 })
                .unwrap();
            graph.upload_buffer("seed", buffer, 0, &[73; 16]).unwrap();
            graph.export_buffer(buffer).unwrap();
            let plan = graph.compile().unwrap();
            let mut pending = runtime.submit(&plan).unwrap();
            let key = pending.export_buffer(buffer).unwrap().key();
            drop(
                pending
                    .wait(Duration::from_secs(30))
                    .unwrap()
                    .expect("GPU completion"),
            );
            key
        };
        runtime
            .prepare_graph_working_set(
                Some(&[D3D12BufferClass { size: 8, count: 1 }]),
                &[],
                &[],
                &[],
            )
            .unwrap();
        {
            let registry = runtime
                .pool
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            assert_eq!(
                registry
                    .entries
                    .iter()
                    .filter(|entry| entry.physical.is_some())
                    .count(),
                2
            );
            assert_eq!(
                registry
                    .entries
                    .iter()
                    .filter(|entry| !entry.registered && entry.fits(buffer(8)))
                    .count(),
                1
            );
            assert!(
                !registry
                    .entries
                    .iter()
                    .any(|entry| !entry.registered && entry.fits(buffer(16)))
            );
        }
        // The old two free 64B instances cannot cover either the old size or
        // a second 8B operand after the exact count/size replacement.
        for sizes in [&[16][..], &[8, 8][..]] {
            let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
            let mut graph = pool.begin().unwrap();
            for &size in sizes {
                let buffer = graph
                    .buffer("absent class/count", BufferDesc { size })
                    .unwrap();
                graph
                    .upload_buffer("seed", buffer, 0, &vec![1; size as usize])
                    .unwrap();
                graph.readback_buffer("observe", buffer, 0, size).unwrap();
            }
            let plan = graph.compile().unwrap();
            assert!(
                runtime
                    .prepare(&plan)
                    .err()
                    .expect("exact class exhaustion")
                    .capacity()
                    .is_some()
            );
        }
        runtime
            .prepare_graph_working_set(Some(&[]), &[], &[], &[])
            .unwrap();
        {
            let registry = runtime
                .pool
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            assert_eq!(
                registry
                    .entries
                    .iter()
                    .filter(|entry| entry.physical.is_some())
                    .count(),
                1
            );
            assert!(
                registry
                    .entries
                    .iter()
                    .filter(|entry| !entry.registered)
                    .all(|entry| !entry.prepared && entry.physical.is_none())
            );
        }
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let buffer = graph
            .buffer("no free storage", BufferDesc { size: 1 })
            .unwrap();
        graph.upload_buffer("seed", buffer, 0, &[1]).unwrap();
        graph.readback_buffer("observe", buffer, 0, 1).unwrap();
        let plan = graph.compile().unwrap();
        assert!(
            runtime
                .prepare(&plan)
                .err()
                .expect("zero free class capacity")
                .capacity()
                .is_some()
        );
        drop((plan, pool));

        let snapshot = runtime.refresh_buffer(key).unwrap();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let buffer = graph
            .import_buffer("preserved registered contents", &snapshot)
            .unwrap();
        let readback = graph.readback_buffer("check", buffer, 0, 16).unwrap();
        let plan = graph.compile().unwrap();
        let mut pending = runtime.submit(&plan).unwrap();
        let mut result = pending
            .wait(Duration::from_secs(30))
            .unwrap()
            .expect("GPU completion");
        let mut bytes = [0; 16];
        result.readback_into(readback, &mut bytes).unwrap();
        assert_eq!(bytes, [73; 16]);
        drop((result, pending, plan, pool, snapshot));
        runtime.shutdown().unwrap();
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:?}");
        if strict {
            assert!(
                report.teardown_checked && report.gpu_based_validation_enabled,
                "{report:?}"
            );
        }
    }
    #[test]
    fn a_partial_mip_chain_is_never_exported_as_fully_initialized() {
        assert!(!fully_initialized(&[], 3));
        assert!(!fully_initialized(&[0..1, 2..3], 3));
        assert!(!fully_initialized(std::slice::from_ref(&(1..3)), 3));
        assert!(fully_initialized(&[0..1, 1..3], 3));
        assert!(fully_initialized(std::slice::from_ref(&(0..3)), 3));
    }
}

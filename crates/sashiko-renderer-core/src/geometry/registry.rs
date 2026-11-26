//! Small, GPU-independent resource and frame-input contracts.
//!
//! The caller owns scene semantics. This module only validates mesh
//! handles and captures immutable data needed to build a frame's graph.

use std::{
    borrow::Cow,
    collections::BTreeSet,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::graph::{BufferHandle, RasterConstants};

use super::GeometryData;
mod graph;
pub use graph::SnapshotInputs;

static NEXT_REGISTRY: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MeshHandle {
    pub(crate) owner: u64,
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshErrorKind {
    InvalidInput,
    CapacityExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshError {
    kind: MeshErrorKind,
    pub message: Cow<'static, str>,
}

impl MeshError {
    pub fn invalid_input(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind: MeshErrorKind::InvalidInput,
            message: message.into(),
        }
    }

    /// A prepared mesh limit was exhausted or its storage could not be reserved.
    pub fn capacity_exhausted(message: &'static str) -> Self {
        Self {
            kind: MeshErrorKind::CapacityExhausted,
            message: Cow::Borrowed(message),
        }
    }

    pub fn kind(&self) -> MeshErrorKind {
        self.kind
    }
}
impl fmt::Display for MeshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for MeshError {}

#[derive(Debug)]
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}
#[derive(Debug)]
struct MeshVersion {
    revision: u64,
    data: Arc<GeometryData>,
}

/// Preparation limits for CPU assets and changed versions awaiting maintenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshCapacity {
    pub meshes: usize,
    /// Maximum updated/deleted mesh versions retained until explicit collection.
    pub retired_mesh_versions: usize,
}

/// Bounded CPU-side assets. Updates preserve a live handle; deletion invalidates
/// it immediately. Reusing a slot creates a different generation.
#[derive(Debug)]
pub struct MeshRegistry {
    pub(crate) id: u64,
    meshes: Vec<Slot<MeshVersion>>,
    free_meshes: BTreeSet<usize>,
    mesh_capacity: usize,
    retired_meshes: Vec<MeshVersion>,
    retired_mesh_capacity: usize,
}

impl MeshRegistry {
    pub fn new(capacity: MeshCapacity) -> Result<Self, MeshError> {
        if capacity.meshes == 0 || capacity.meshes > u32::MAX as usize {
            return Err(MeshError::invalid_input(
                "mesh capacity must be nonzero and fit u32",
            ));
        }
        let id = NEXT_REGISTRY
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| MeshError::invalid_input("mesh registry identity exhausted"))?;
        let mut meshes = Vec::new();
        let mut retired_meshes = Vec::new();
        meshes
            .try_reserve_exact(capacity.meshes)
            .map_err(|_| MeshError::capacity_exhausted("unable to reserve mesh slots"))?;
        retired_meshes
            .try_reserve_exact(capacity.retired_mesh_versions)
            .map_err(|_| MeshError::capacity_exhausted("unable to reserve retired mesh slots"))?;
        Ok(Self {
            id,
            meshes,
            free_meshes: BTreeSet::new(),
            mesh_capacity: capacity.meshes,
            retired_meshes,
            retired_mesh_capacity: capacity.retired_mesh_versions,
        })
    }

    pub fn create_mesh(&mut self, data: GeometryData) -> Result<MeshHandle, MeshError> {
        let (index, generation) = insert(
            &mut self.meshes,
            &mut self.free_meshes,
            self.mesh_capacity,
            MeshVersion {
                revision: 1,
                data: Arc::new(data),
            },
        )?;
        Ok(MeshHandle {
            owner: self.id,
            index,
            generation,
        })
    }
    pub fn update_mesh(&mut self, handle: MeshHandle, data: GeometryData) -> Result<(), MeshError> {
        self.check_owner(handle.owner)?;
        let mesh = lookup_mut(&mut self.meshes, handle.index, handle.generation)?;
        let revision = mesh
            .revision
            .checked_add(1)
            .ok_or_else(|| MeshError::invalid_input("mesh revision exhausted"))?;
        if self.retired_meshes.len() == self.retired_mesh_capacity {
            return Err(MeshError::capacity_exhausted(
                "retired mesh capacity exhausted",
            ));
        }
        let previous = std::mem::replace(
            mesh,
            MeshVersion {
                revision,
                data: Arc::new(data),
            },
        );
        self.retired_meshes.push(previous);
        Ok(())
    }
    pub fn delete_mesh(&mut self, handle: MeshHandle) -> Result<(), MeshError> {
        self.check_owner(handle.owner)?;
        lookup(&self.meshes, handle.index, handle.generation)?;
        if self.retired_meshes.len() == self.retired_mesh_capacity {
            return Err(MeshError::capacity_exhausted(
                "retired mesh capacity exhausted",
            ));
        }
        let previous = self.meshes[handle.index as usize]
            .value
            .take()
            .expect("lookup established a live mesh");
        self.retired_meshes.push(previous);
        if handle.generation < u32::MAX {
            self.free_meshes.insert(handle.index as usize);
        }
        Ok(())
    }

    /// Current retained versions and configured limit for atomic batch retirement.
    pub fn retirement_usage(&self) -> (usize, usize) {
        (self.retired_meshes.len(), self.retired_mesh_capacity)
    }

    /// Reclaim changed mesh versions at an explicit maintenance boundary.
    ///
    /// Snapshots and graph uploads retain different `Arc`s. An unreferenced
    /// version can be released only while each stream is exclusive or another
    /// version retained by this MeshRegistry anchors it. Sequential removal keeps the
    /// final anchor while graph uploads or cloned `GeometryData` still retain bytes.
    /// No weak references to these owners are exposed.
    /// Keep this MeshRegistry alive throughout ordinary frame processing; destroying
    /// it starts shutdown, including the eventual release of surviving frames.
    pub fn collect_retired_meshes(&mut self) -> usize {
        let before = self.retired_meshes.len();
        let mut index = 0;
        while index < self.retired_meshes.len() {
            let version = &self.retired_meshes[index];
            if Arc::strong_count(&version.data) == 1
                && self.stream_is_anchored(&version.data.vertices, index)
                && self.stream_is_anchored(&version.data.indices, index)
            {
                self.retired_meshes.swap_remove(index);
            } else {
                index += 1;
            }
        }
        before - self.retired_meshes.len()
    }

    /// Transfer a native source anchor only to a version this asset owner keeps
    /// until explicit collection (or shutdown), rather than to a frame snapshot.
    #[cfg(feature = "vulkan")]
    pub(crate) fn anchors_mesh_data(&self, data: &Arc<GeometryData>) -> bool {
        self.meshes
            .iter()
            .filter_map(|slot| slot.value.as_ref())
            .chain(&self.retired_meshes)
            .any(|version| Arc::ptr_eq(&version.data, data))
    }

    fn stream_is_anchored(&self, bytes: &Arc<[u8]>, candidate: usize) -> bool {
        Arc::strong_count(bytes) == 1
            || self
                .meshes
                .iter()
                .filter_map(|slot| slot.value.as_ref())
                .chain(
                    self.retired_meshes
                        .iter()
                        .enumerate()
                        .filter_map(|(index, version)| (index != candidate).then_some(version)),
                )
                .any(|version| {
                    Arc::ptr_eq(bytes, &version.data.vertices)
                        || Arc::ptr_eq(bytes, &version.data.indices)
                })
    }
    /// Borrow the current immutable geometry at an asset preparation boundary.
    pub fn geometry(&self, handle: MeshHandle) -> Result<&GeometryData, MeshError> {
        self.check_owner(handle.owner)?;
        Ok(&lookup(&self.meshes, handle.index, handle.generation)?.data)
    }

    pub fn mesh_revision(&self, handle: MeshHandle) -> Result<u64, MeshError> {
        self.check_owner(handle.owner)?;
        Ok(lookup(&self.meshes, handle.index, handle.generation)?.revision)
    }
    pub fn live_mesh_handles(&self) -> impl Iterator<Item = MeshHandle> + '_ {
        self.meshes.iter().enumerate().filter_map(|(index, slot)| {
            slot.value.as_ref().map(|_| MeshHandle {
                owner: self.id,
                index: index as u32,
                generation: slot.generation,
            })
        })
    }

    /// Capture immutable mesh versions and the draw constants in bounded storage.
    pub fn capture<T>(
        &self,
        pool: &SnapshotPool,
        draws: &[T],
        mesh_handle: impl Fn(&T) -> MeshHandle,
        constants: impl Fn(&T, &GeometryData) -> Result<RasterConstants, MeshError>,
    ) -> Result<MeshSnapshot, MeshError> {
        if draws.len() > pool.owner.draw_capacity {
            return Err(MeshError::capacity_exhausted(
                "snapshot draw capacity exhausted",
            ));
        }
        // Construct the lease before fallible capture so every early return
        // returns its storage without freeing the prepared arrays.
        let mut captured = pool.acquire()?;
        for draw in draws {
            let handle = mesh_handle(draw);
            self.check_owner(handle.owner)?;
            let mesh = lookup(&self.meshes, handle.index, handle.generation)?;
            let parameters = constants(draw, &mesh.data)?;
            captured.draws.push(SnapshotDraw {
                mesh: handle,
                revision: mesh.revision,
                data: Arc::clone(&mesh.data),
                parameters,
            });
        }
        Ok(captured)
    }

    fn check_owner(&self, owner: u64) -> Result<(), MeshError> {
        if owner == self.id {
            Ok(())
        } else {
            Err(MeshError::invalid_input("mesh belongs to another registry"))
        }
    }
}

/// Owned frame input; it does not borrow a mutable scene or promise GPU completion.
#[derive(Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(
        dead_code,
        reason = "captured frame operands are consumed by the optional Vulkan bridge"
    )
)]
pub struct MeshSnapshot {
    pub(crate) draws: Vec<SnapshotDraw>,
    meshes: Mutex<Option<Vec<SnapshotMesh>>>,
    pool: Arc<SnapshotPoolOwner>,
}
impl MeshSnapshot {
    pub fn draw_count(&self) -> usize {
        self.draws.len()
    }
}

impl Drop for MeshSnapshot {
    fn drop(&mut self) {
        let mut draws = std::mem::take(&mut self.draws);
        draws.clear();
        let mut meshes = self
            .meshes
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or_default();
        meshes.clear();
        self.pool
            .free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(SnapshotStorage { draws, meshes });
    }
}

/// Prepared, bounded storage for independently owned frame captures.
///
/// Capture and return reuse these arrays even when draw counts and values vary.
/// Keep an owning pool handle and the source MeshRegistry alive until all ordinary
/// frame consumers finish. Dropping those owners is an explicit shutdown
/// boundary; outstanding snapshots remain memory-safe and release storage later.
#[derive(Clone, Debug)]
pub struct SnapshotPool {
    owner: Arc<SnapshotPoolOwner>,
}

#[derive(Debug)]
struct SnapshotPoolOwner {
    draw_capacity: usize,
    free: Mutex<Vec<SnapshotStorage>>,
}

#[derive(Debug)]
struct SnapshotStorage {
    draws: Vec<SnapshotDraw>,
    meshes: Vec<SnapshotMesh>,
}

#[derive(Debug)]
struct SnapshotMesh {
    mesh: MeshHandle,
    draw: usize,
    buffers: Option<(BufferHandle, BufferHandle)>,
}

impl SnapshotPool {
    pub fn new(draw_capacity: usize, slots: usize) -> Result<Self, MeshError> {
        if slots == 0 {
            return Err(MeshError::invalid_input(
                "snapshot pool needs a nonzero slot count",
            ));
        }
        let mut free = Vec::new();
        free.try_reserve_exact(slots)
            .map_err(|_| MeshError::capacity_exhausted("unable to reserve snapshot slots"))?;
        for _ in 0..slots {
            let mut draws = Vec::new();
            let mut meshes = Vec::new();
            draws
                .try_reserve_exact(draw_capacity)
                .map_err(|_| MeshError::capacity_exhausted("unable to reserve snapshot draws"))?;
            meshes
                .try_reserve_exact(draw_capacity)
                .map_err(|_| MeshError::capacity_exhausted("unable to reserve snapshot meshes"))?;
            free.push(SnapshotStorage { draws, meshes });
        }
        Ok(Self {
            owner: Arc::new(SnapshotPoolOwner {
                draw_capacity,
                free: Mutex::new(free),
            }),
        })
    }

    fn acquire(&self) -> Result<MeshSnapshot, MeshError> {
        let storage = self
            .owner
            .free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
            .ok_or_else(|| MeshError::capacity_exhausted("snapshot slots exhausted"))?;
        Ok(MeshSnapshot {
            draws: storage.draws,
            meshes: Mutex::new(Some(storage.meshes)),
            pool: Arc::clone(&self.owner),
        })
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(
        dead_code,
        reason = "captured draw operands are consumed by the optional Vulkan bridge"
    )
)]
pub(crate) struct SnapshotDraw {
    pub mesh: MeshHandle,
    pub revision: u64,
    pub data: Arc<GeometryData>,
    pub parameters: RasterConstants,
}

fn insert<T>(
    slots: &mut Vec<Slot<T>>,
    free: &mut BTreeSet<usize>,
    capacity: usize,
    value: T,
) -> Result<(u32, u32), MeshError> {
    // Deletion records reusable slots. Cold registration never scans live
    // assets; reuse preserves the previous lowest-eligible-index policy.
    if let Some(index) = free.pop_first() {
        let slot = &mut slots[index];
        debug_assert!(slot.value.is_none() && slot.generation < u32::MAX);
        slot.generation += 1;
        slot.value = Some(value);
        return Ok((index as u32, slot.generation));
    }
    if slots.len() >= capacity {
        return Err(MeshError::capacity_exhausted("asset capacity exhausted"));
    }
    let index = slots.len() as u32;
    slots.push(Slot {
        generation: 1,
        value: Some(value),
    });
    Ok((index, 1))
}
fn lookup<T>(slots: &[Slot<T>], index: u32, generation: u32) -> Result<&T, MeshError> {
    slots
        .get(index as usize)
        .filter(|slot| slot.generation == generation)
        .and_then(|slot| slot.value.as_ref())
        .ok_or_else(|| MeshError::invalid_input("deleted or stale asset handle"))
}
fn lookup_mut<T>(slots: &mut [Slot<T>], index: u32, generation: u32) -> Result<&mut T, MeshError> {
    slots
        .get_mut(index as usize)
        .filter(|slot| slot.generation == generation)
        .and_then(|slot| slot.value.as_mut())
        .ok_or_else(|| MeshError::invalid_input("deleted or stale asset handle"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity(meshes: usize) -> MeshCapacity {
        MeshCapacity {
            meshes,
            retired_mesh_versions: meshes.max(2),
        }
    }

    fn mesh(value: f32) -> GeometryData {
        GeometryData::new(4, &value.to_le_bytes(), &[0; 3]).unwrap()
    }

    fn capture(
        assets: &MeshRegistry,
        pool: &SnapshotPool,
        meshes: &[MeshHandle],
        parameters: RasterConstants,
    ) -> Result<MeshSnapshot, MeshError> {
        assets.capture(pool, meshes, |mesh| *mesh, |_, _| Ok(parameters))
    }

    #[test]
    fn handles_validate_owner_generation_and_capacity() {
        let mut first = MeshRegistry::new(capacity(1)).unwrap();
        let second = MeshRegistry::new(capacity(1)).unwrap();
        let old = first.create_mesh(mesh(1.0)).unwrap();
        assert!(first.create_mesh(mesh(0.0)).is_err());
        assert!(second.mesh_revision(old).is_err());
        first.delete_mesh(old).unwrap();
        assert!(first.delete_mesh(old).is_err());
        let new = first.create_mesh(mesh(0.5)).unwrap();
        assert_ne!(old, new);
        assert!(first.mesh_revision(old).is_err());
        assert_eq!(first.mesh_revision(new).unwrap(), 1);
    }

    #[test]
    fn large_registration_reuses_lowest_deleted_indices_independently() {
        const COUNT: usize = 4_096;
        let mut engine = MeshRegistry::new(capacity(COUNT)).unwrap();
        let data = mesh(1.0);
        let meshes = (0..COUNT)
            .map(|_| engine.create_mesh(data.clone()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(meshes.last().unwrap().index, (COUNT - 1) as u32);
        assert!(engine.create_mesh(data.clone()).is_err());
        for index in [3_072, 1, 2_048, 0] {
            engine.delete_mesh(meshes[index]).unwrap();
        }
        for index in [0, 1, 2_048, 3_072] {
            let new_mesh = engine.create_mesh(data.clone()).unwrap();
            assert_eq!(new_mesh.index, index as u32);
            assert_eq!(new_mesh.generation, 2);
            assert!(engine.delete_mesh(meshes[index]).is_err());
            assert_eq!(engine.mesh_revision(new_mesh).unwrap(), 1);
        }
        assert_eq!(engine.live_mesh_handles().count(), COUNT);
        assert!(engine.create_mesh(data).is_err());
    }

    #[test]
    fn generation_exhausted_slots_are_retired_instead_of_reused() {
        let mut engine = MeshRegistry::new(capacity(2)).unwrap();
        for expected_index in 0..2 {
            let mut mesh_handle = engine.create_mesh(mesh(1.0)).unwrap();
            assert_eq!(mesh_handle.index, expected_index);
            engine.meshes[mesh_handle.index as usize].generation = u32::MAX;
            mesh_handle.generation = u32::MAX;
            engine.delete_mesh(mesh_handle).unwrap();
        }
        assert!(engine.create_mesh(mesh(1.0)).is_err());
        assert!(engine.live_mesh_handles().next().is_none());
    }

    #[test]
    fn failed_mutations_do_not_consume_reusable_slots_or_replace_live_data() {
        let mut engine = MeshRegistry::new(capacity(1)).unwrap();
        let mesh_handle = engine.create_mesh(mesh(1.0)).unwrap();
        let slot = engine.meshes[mesh_handle.index as usize]
            .value
            .as_mut()
            .unwrap();
        slot.revision = u64::MAX;
        let old_data = Arc::clone(&slot.data);
        assert!(engine.update_mesh(mesh_handle, mesh(0.0)).is_err());
        assert_eq!(engine.mesh_revision(mesh_handle).unwrap(), u64::MAX);
        assert!(Arc::ptr_eq(
            &old_data,
            &engine.meshes[mesh_handle.index as usize]
                .value
                .as_ref()
                .unwrap()
                .data,
        ));
        let other = MeshRegistry::new(capacity(1)).unwrap();
        assert!(
            engine
                .delete_mesh(MeshHandle {
                    owner: other.id,
                    ..mesh_handle
                })
                .is_err()
        );
        assert!(engine.create_mesh(mesh(0.0)).is_err());
    }

    #[test]
    fn snapshots_keep_their_mesh_version_and_draw_constants() {
        let pool = SnapshotPool::new(1, 2).unwrap();
        let mut assets = MeshRegistry::new(capacity(1)).unwrap();
        let mesh_handle = assets.create_mesh(mesh(1.0)).unwrap();
        let first = RasterConstants::new(&[11, 12]).unwrap();
        let second = RasterConstants::new(&[21]).unwrap();
        let old = capture(&assets, &pool, &[mesh_handle], first).unwrap();
        assets.update_mesh(mesh_handle, mesh(0.0)).unwrap();
        let new = capture(&assets, &pool, &[mesh_handle], second).unwrap();
        assets.delete_mesh(mesh_handle).unwrap();

        assert_eq!(old.draws[0].revision, 1);
        assert_eq!(new.draws[0].revision, 2);
        assert_eq!(old.draws[0].parameters, first);
        assert_eq!(new.draws[0].parameters, second);
        assert_eq!(&*old.draws[0].data.vertices, &1.0f32.to_le_bytes());
        assert_eq!(&*new.draws[0].data.vertices, &0.0f32.to_le_bytes());
        assert_eq!(old.draws[0].data.index_count, 3);
        assert_eq!(old.draws[0].data.indices.len(), 12);
    }

    #[test]
    fn snapshot_slots_reuse_capacity_and_return_after_partial_failure() {
        let pool = SnapshotPool::new(2, 1).unwrap();
        let mut engine = MeshRegistry::new(capacity(1)).unwrap();
        let mesh = engine.create_mesh(mesh(1.0)).unwrap();
        let snapshot =
            |meshes: &[MeshHandle]| capture(&engine, &pool, meshes, RasterConstants::default());
        let frame = snapshot(&[mesh; 2]).unwrap();
        let storage = frame.draws.as_ptr();
        assert_eq!(
            snapshot(&[]).unwrap_err().kind(),
            MeshErrorKind::CapacityExhausted,
        );
        drop(frame);
        let foreign = MeshHandle { owner: 0, ..mesh };
        assert!(snapshot(&[mesh, foreign]).is_err());
        let frame = snapshot(&[mesh]).unwrap();
        assert_eq!(frame.draws.as_ptr(), storage);
        assert_eq!(frame.draw_count(), 1);
        drop(frame);
        assert_eq!(
            snapshot(&[mesh; 3]).unwrap_err().kind(),
            MeshErrorKind::CapacityExhausted,
        );
        assert_eq!(snapshot(&[]).unwrap().draw_count(), 0);
    }

    #[test]
    fn retired_meshes_wait_for_snapshot_and_both_upload_streams() {
        let pool = SnapshotPool::new(1, 1).unwrap();
        let mut engine = MeshRegistry::new(MeshCapacity {
            meshes: 1,
            retired_mesh_versions: 1,
        })
        .unwrap();
        let handle = engine.create_mesh(mesh(1.0)).unwrap();
        let frame = capture(&engine, &pool, &[handle], RasterConstants::default()).unwrap();
        let vertices = Arc::clone(&frame.draws[0].data.vertices);
        let indices = Arc::clone(&frame.draws[0].data.indices);
        engine.update_mesh(handle, mesh(0.5)).unwrap();
        assert_eq!(engine.collect_retired_meshes(), 0);
        assert_eq!(
            engine.update_mesh(handle, mesh(0.0)).unwrap_err().kind(),
            MeshErrorKind::CapacityExhausted,
        );
        assert_eq!(engine.mesh_revision(handle).unwrap(), 2);
        assert_eq!(
            engine.delete_mesh(handle).unwrap_err().kind(),
            MeshErrorKind::CapacityExhausted,
        );
        drop(frame);
        assert_eq!(engine.collect_retired_meshes(), 0);
        drop(vertices);
        assert_eq!(engine.collect_retired_meshes(), 0);
        drop(indices);
        assert_eq!(engine.collect_retired_meshes(), 1);
        engine.delete_mesh(handle).unwrap();
        assert_eq!(engine.collect_retired_meshes(), 1);
        assert!(engine.mesh_revision(handle).is_err());
    }

    #[test]
    fn maintenance_releases_shared_versions_but_keeps_the_last_stream_anchor() {
        let mut engine = MeshRegistry::new(MeshCapacity {
            meshes: 1,
            retired_mesh_versions: 2,
        })
        .unwrap();
        let data = mesh(1.0);
        let handle = engine.create_mesh(data.clone()).unwrap();
        engine.update_mesh(handle, data.clone()).unwrap();
        assert_eq!(engine.collect_retired_meshes(), 1);
        engine.update_mesh(handle, data.clone()).unwrap();
        engine.delete_mesh(handle).unwrap();
        assert_eq!(engine.collect_retired_meshes(), 1);
        assert_eq!(engine.retired_meshes.len(), 1);
        drop(data);
        assert_eq!(engine.collect_retired_meshes(), 1);
    }
}

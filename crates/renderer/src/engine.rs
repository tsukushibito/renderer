//! Small, GPU-independent resource and frame-input contracts.
//!
//! The caller owns scene semantics. This module only validates mesh/material
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

use crate::graph::{BufferHandle, RasterParameters};

mod graph;
pub use graph::{FrameGraph, FrameOutputOwnership, SnapshotInputs};

/// Four columns, each containing four rows. Vectors are column vectors.
/// `clip_from_world * world_from_model` transforms a local vertex to clip space.
pub type Matrix4 = [[f32; 4]; 4];

pub const IDENTITY: Matrix4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    /// Linear RGBA; no implicit sRGB decoding is performed.
    pub color: [f32; 4],
}

/// Validated, immutable mesh input. Rust struct layout is never a shader ABI.
#[derive(Clone, Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(
        dead_code,
        reason = "encoded mesh bytes are consumed by the optional Vulkan bridge"
    )
)]
pub struct MeshData {
    pub(crate) vertices: Arc<[u8]>,
    pub(crate) indices: Arc<[u8]>,
    pub(crate) index_count: u32,
    pub(crate) index_maximum: u32,
}

impl MeshData {
    /// Encoded vertex and index payload bytes, independent of sharing and native allocation size.
    pub fn byte_len(&self) -> u64 {
        self.vertices.len() as u64 + self.indices.len() as u64
    }

    pub fn new(vertices: &[Vertex], indices: &[u32]) -> Result<Self, EngineError> {
        let index_count = u32::try_from(indices.len())
            .map_err(|_| EngineError::new("mesh index count exceeds u32"))?;
        if vertices.is_empty() || indices.is_empty() || !indices.len().is_multiple_of(3) {
            return Err(EngineError::new(
                "mesh needs vertices and nonzero triangle indices",
            ));
        }
        if vertices
            .iter()
            .any(|v| v.position.iter().chain(&v.color).any(|x| !x.is_finite()))
        {
            return Err(EngineError::new("mesh vertex components must be finite"));
        }
        let index_maximum = indices.iter().copied().max().expect("nonempty indices");
        if index_maximum as usize >= vertices.len() {
            return Err(EngineError::new("mesh index exceeds its vertex count"));
        }
        let vertex_bytes = vertices
            .len()
            .checked_mul(28)
            .ok_or_else(|| EngineError::new("mesh vertex byte size overflow"))?;
        let index_bytes = indices
            .len()
            .checked_mul(4)
            .ok_or_else(|| EngineError::new("mesh index byte size overflow"))?;
        let mut encoded_vertices = Vec::with_capacity(vertex_bytes);
        for vertex in vertices {
            for value in vertex.position.iter().chain(&vertex.color) {
                encoded_vertices.extend_from_slice(&value.to_ne_bytes());
            }
        }
        let mut encoded_indices = Vec::with_capacity(index_bytes);
        for index in indices {
            encoded_indices.extend_from_slice(&index.to_ne_bytes());
        }
        Ok(Self {
            vertices: encoded_vertices.into(),
            indices: encoded_indices.into(),
            index_count,
            index_maximum,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MeshHandle {
    pub(crate) owner: u64,
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MaterialHandle {
    owner: u64,
    index: u32,
    generation: u32,
}

/// The initial material is a multiplicative linear RGBA tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub tint: [f32; 4],
}

impl Default for Material {
    fn default() -> Self {
        Self { tint: [1.0; 4] }
    }
}

/// A complete offscreen view. Clip depth is 0..W; viewport height is positive.
/// Projection/handedness are chosen by the caller through `clip_from_world`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub width: u32,
    pub height: u32,
    pub clip_from_world: Matrix4,
    pub clear_color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct Draw {
    pub mesh: MeshHandle,
    pub material: MaterialHandle,
    pub world_from_model: Matrix4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineErrorKind {
    InvalidInput,
    CapacityExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineError {
    kind: EngineErrorKind,
    pub message: Cow<'static, str>,
}

impl EngineError {
    pub(crate) fn new(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind: EngineErrorKind::InvalidInput,
            message: message.into(),
        }
    }

    fn capacity(message: &'static str) -> Self {
        Self {
            kind: EngineErrorKind::CapacityExhausted,
            message: Cow::Borrowed(message),
        }
    }

    pub fn kind(&self) -> EngineErrorKind {
        self.kind
    }
}
impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for EngineError {}

#[derive(Debug)]
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}
#[derive(Debug)]
struct MeshVersion {
    revision: u64,
    data: Arc<MeshData>,
}

/// Preparation limits for CPU assets and changed versions awaiting maintenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineCapacity {
    pub meshes: usize,
    pub materials: usize,
    /// Maximum updated/deleted mesh versions retained until explicit collection.
    pub retired_mesh_versions: usize,
}

/// Bounded CPU-side assets. Updates preserve a live handle; deletion invalidates
/// it immediately. Reusing a slot creates a different generation.
#[derive(Debug)]
pub struct Engine {
    pub(crate) id: u64,
    meshes: Vec<Slot<MeshVersion>>,
    materials: Vec<Slot<Material>>,
    free_meshes: BTreeSet<usize>,
    free_materials: BTreeSet<usize>,
    mesh_capacity: usize,
    material_capacity: usize,
    retired_meshes: Vec<MeshVersion>,
    retired_mesh_capacity: usize,
}

impl Engine {
    pub fn new(capacity: EngineCapacity) -> Result<Self, EngineError> {
        if capacity.meshes == 0
            || capacity.materials == 0
            || capacity.meshes > u32::MAX as usize
            || capacity.materials > u32::MAX as usize
        {
            return Err(EngineError::new(
                "asset capacities must be nonzero and fit u32",
            ));
        }
        let id = NEXT_ENGINE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| EngineError::new("engine identity exhausted"))?;
        let mut meshes = Vec::new();
        let mut materials = Vec::new();
        let mut retired_meshes = Vec::new();
        meshes
            .try_reserve_exact(capacity.meshes)
            .map_err(|_| EngineError::capacity("unable to reserve mesh slots"))?;
        materials
            .try_reserve_exact(capacity.materials)
            .map_err(|_| EngineError::capacity("unable to reserve material slots"))?;
        retired_meshes
            .try_reserve_exact(capacity.retired_mesh_versions)
            .map_err(|_| EngineError::capacity("unable to reserve retired mesh slots"))?;
        Ok(Self {
            id,
            meshes,
            materials,
            free_meshes: BTreeSet::new(),
            free_materials: BTreeSet::new(),
            mesh_capacity: capacity.meshes,
            material_capacity: capacity.materials,
            retired_meshes,
            retired_mesh_capacity: capacity.retired_mesh_versions,
        })
    }

    pub fn create_mesh(&mut self, data: MeshData) -> Result<MeshHandle, EngineError> {
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
    pub fn update_mesh(&mut self, handle: MeshHandle, data: MeshData) -> Result<(), EngineError> {
        self.check_owner(handle.owner)?;
        let mesh = lookup_mut(&mut self.meshes, handle.index, handle.generation)?;
        let revision = mesh
            .revision
            .checked_add(1)
            .ok_or_else(|| EngineError::new("mesh revision exhausted"))?;
        if self.retired_meshes.len() == self.retired_mesh_capacity {
            return Err(EngineError::capacity("retired mesh capacity exhausted"));
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
    pub fn delete_mesh(&mut self, handle: MeshHandle) -> Result<(), EngineError> {
        self.check_owner(handle.owner)?;
        lookup(&self.meshes, handle.index, handle.generation)?;
        if self.retired_meshes.len() == self.retired_mesh_capacity {
            return Err(EngineError::capacity("retired mesh capacity exhausted"));
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

    /// Reclaim changed mesh versions at an explicit maintenance boundary.
    ///
    /// Snapshots and graph uploads retain different `Arc`s. An unreferenced
    /// version can be released only while each stream is exclusive or another
    /// version retained by this Engine anchors it. Sequential removal keeps the
    /// final anchor while graph uploads or cloned `MeshData` still retain bytes.
    /// No weak references to these owners are exposed.
    /// Keep this Engine alive throughout ordinary frame processing; destroying
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
    pub(crate) fn anchors_mesh_data(&self, data: &Arc<MeshData>) -> bool {
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
    pub fn mesh_revision(&self, handle: MeshHandle) -> Result<u64, EngineError> {
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

    pub fn create_material(&mut self, material: Material) -> Result<MaterialHandle, EngineError> {
        validate_material(material)?;
        let (index, generation) = insert(
            &mut self.materials,
            &mut self.free_materials,
            self.material_capacity,
            material,
        )?;
        Ok(MaterialHandle {
            owner: self.id,
            index,
            generation,
        })
    }
    pub fn update_material(
        &mut self,
        handle: MaterialHandle,
        material: Material,
    ) -> Result<(), EngineError> {
        self.check_owner(handle.owner)?;
        validate_material(material)?;
        *lookup_mut(&mut self.materials, handle.index, handle.generation)? = material;
        Ok(())
    }
    pub fn delete_material(&mut self, handle: MaterialHandle) -> Result<(), EngineError> {
        self.check_owner(handle.owner)?;
        lookup(&self.materials, handle.index, handle.generation)?;
        self.materials[handle.index as usize].value = None;
        if handle.generation < u32::MAX {
            self.free_materials.insert(handle.index as usize);
        }
        Ok(())
    }

    /// Capture owns each immutable mesh version and copies material/matrices.
    /// Later CPU asset updates cannot change an already captured frame.
    pub fn snapshot(
        &self,
        pool: &SnapshotPool,
        view: &View,
        draws: &[Draw],
    ) -> Result<FrameSnapshot, EngineError> {
        if view.width == 0
            || view.height == 0
            || u64::from(view.width)
                .checked_mul(u64::from(view.height))
                .and_then(|n| n.checked_mul(4))
                .is_none()
            || !finite_matrix(view.clip_from_world)
            || view.clear_color.iter().any(|x| !x.is_finite())
        {
            return Err(EngineError::new(
                "view needs a valid extent and finite parameters",
            ));
        }
        if draws.len() > pool.owner.draw_capacity {
            return Err(EngineError::capacity("snapshot draw capacity exhausted"));
        }
        // Construct the lease before fallible capture so every early return
        // returns its storage without freeing the prepared arrays.
        let mut captured = pool.acquire(*view)?;
        for draw in draws {
            self.check_owner(draw.mesh.owner)?;
            self.check_owner(draw.material.owner)?;
            let mesh = lookup(&self.meshes, draw.mesh.index, draw.mesh.generation)?;
            let material = lookup(
                &self.materials,
                draw.material.index,
                draw.material.generation,
            )?;
            if !finite_matrix(draw.world_from_model) {
                return Err(EngineError::new("model matrix must be finite"));
            }
            let transform = multiply(view.clip_from_world, draw.world_from_model);
            let parameters = RasterParameters::new(transform, material.tint)
                .map_err(|_| EngineError::new("combined clip transform is not finite"))?;
            captured.draws.push(SnapshotDraw {
                mesh: draw.mesh,
                revision: mesh.revision,
                data: Arc::clone(&mesh.data),
                parameters,
            });
        }
        Ok(captured)
    }

    fn check_owner(&self, owner: u64) -> Result<(), EngineError> {
        if owner == self.id {
            Ok(())
        } else {
            Err(EngineError::new("asset belongs to another engine"))
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
pub struct FrameSnapshot {
    pub(crate) view: View,
    pub(crate) draws: Vec<SnapshotDraw>,
    meshes: Mutex<Option<Vec<SnapshotMesh>>>,
    pool: Arc<SnapshotPoolOwner>,
}
impl FrameSnapshot {
    pub fn view(&self) -> &View {
        &self.view
    }
    pub fn draw_count(&self) -> usize {
        self.draws.len()
    }
}

impl Drop for FrameSnapshot {
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
/// Keep an owning pool handle and the source Engine alive until all ordinary
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
    pub fn new(draw_capacity: usize, slots: usize) -> Result<Self, EngineError> {
        if slots == 0 {
            return Err(EngineError::new("snapshot pool needs a nonzero slot count"));
        }
        let mut free = Vec::new();
        free.try_reserve_exact(slots)
            .map_err(|_| EngineError::capacity("unable to reserve snapshot slots"))?;
        for _ in 0..slots {
            let mut draws = Vec::new();
            let mut meshes = Vec::new();
            draws
                .try_reserve_exact(draw_capacity)
                .map_err(|_| EngineError::capacity("unable to reserve snapshot draws"))?;
            meshes
                .try_reserve_exact(draw_capacity)
                .map_err(|_| EngineError::capacity("unable to reserve snapshot meshes"))?;
            free.push(SnapshotStorage { draws, meshes });
        }
        Ok(Self {
            owner: Arc::new(SnapshotPoolOwner {
                draw_capacity,
                free: Mutex::new(free),
            }),
        })
    }

    fn acquire(&self, view: View) -> Result<FrameSnapshot, EngineError> {
        let storage = self
            .owner
            .free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
            .ok_or_else(|| EngineError::capacity("snapshot slots exhausted"))?;
        Ok(FrameSnapshot {
            view,
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
    pub data: Arc<MeshData>,
    pub parameters: RasterParameters,
}

fn validate_material(material: Material) -> Result<(), EngineError> {
    if material.tint.iter().all(|x| x.is_finite()) {
        Ok(())
    } else {
        Err(EngineError::new("material tint must be finite"))
    }
}
fn finite_matrix(matrix: Matrix4) -> bool {
    matrix.iter().flatten().all(|x| x.is_finite())
}
fn multiply(a: Matrix4, b: Matrix4) -> Matrix4 {
    let mut output = [[0.0; 4]; 4];
    for (column, result) in output.iter_mut().enumerate() {
        for (row, value) in result.iter_mut().enumerate() {
            *value = (0..4).map(|k| a[k][row] * b[column][k]).sum();
        }
    }
    output
}
fn insert<T>(
    slots: &mut Vec<Slot<T>>,
    free: &mut BTreeSet<usize>,
    capacity: usize,
    value: T,
) -> Result<(u32, u32), EngineError> {
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
        return Err(EngineError::capacity("asset capacity exhausted"));
    }
    let index = slots.len() as u32;
    slots.push(Slot {
        generation: 1,
        value: Some(value),
    });
    Ok((index, 1))
}
fn lookup<T>(slots: &[Slot<T>], index: u32, generation: u32) -> Result<&T, EngineError> {
    slots
        .get(index as usize)
        .filter(|slot| slot.generation == generation)
        .and_then(|slot| slot.value.as_ref())
        .ok_or_else(|| EngineError::new("deleted or stale asset handle"))
}
fn lookup_mut<T>(
    slots: &mut [Slot<T>],
    index: u32,
    generation: u32,
) -> Result<&mut T, EngineError> {
    slots
        .get_mut(index as usize)
        .filter(|slot| slot.generation == generation)
        .and_then(|slot| slot.value.as_mut())
        .ok_or_else(|| EngineError::new("deleted or stale asset handle"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity(meshes: usize, materials: usize) -> EngineCapacity {
        EngineCapacity {
            meshes,
            materials,
            retired_mesh_versions: meshes.max(2),
        }
    }

    fn mesh(red: f32) -> MeshData {
        MeshData::new(
            &[Vertex {
                position: [0.0; 3],
                color: [red, 0.0, 0.0, 1.0],
            }],
            &[0, 0, 0],
        )
        .unwrap()
    }
    fn view() -> View {
        View {
            width: 8,
            height: 8,
            clip_from_world: IDENTITY,
            clear_color: [0.0; 4],
        }
    }

    #[test]
    fn handles_validate_owner_generation_and_capacity() {
        assert_eq!(mesh(1.0).byte_len(), 28 + 3 * 4);
        let mut first = Engine::new(capacity(1, 1)).unwrap();
        let second = Engine::new(capacity(1, 1)).unwrap();
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
        let mut engine = Engine::new(capacity(COUNT, COUNT)).unwrap();
        let data = mesh(1.0);
        let meshes = (0..COUNT)
            .map(|_| engine.create_mesh(data.clone()).unwrap())
            .collect::<Vec<_>>();
        let materials = (0..COUNT)
            .map(|_| engine.create_material(Material::default()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(meshes.last().unwrap().index, (COUNT - 1) as u32);
        assert_eq!(materials.last().unwrap().index, (COUNT - 1) as u32);
        assert!(engine.create_mesh(data.clone()).is_err());
        assert!(engine.create_material(Material::default()).is_err());
        for index in [3_072, 1, 2_048, 0] {
            engine.delete_mesh(meshes[index]).unwrap();
            engine.delete_material(materials[index]).unwrap();
        }
        for index in [0, 1, 2_048, 3_072] {
            let new_mesh = engine.create_mesh(data.clone()).unwrap();
            let new_material = engine.create_material(Material::default()).unwrap();
            assert_eq!(new_mesh.index, index as u32);
            assert_eq!(new_material.index, index as u32);
            assert_eq!(new_mesh.generation, 2);
            assert_eq!(new_material.generation, 2);
            assert!(engine.delete_mesh(meshes[index]).is_err());
            assert!(engine.delete_material(materials[index]).is_err());
            assert_eq!(engine.mesh_revision(new_mesh).unwrap(), 1);
        }
        assert_eq!(engine.live_mesh_handles().count(), COUNT);
        assert!(engine.create_mesh(data).is_err());
        assert!(engine.create_material(Material::default()).is_err());
    }

    #[test]
    fn generation_exhausted_slots_are_retired_instead_of_reused() {
        let mut engine = Engine::new(capacity(2, 2)).unwrap();
        for expected_index in 0..2 {
            let mut mesh_handle = engine.create_mesh(mesh(1.0)).unwrap();
            let mut material = engine.create_material(Material::default()).unwrap();
            assert_eq!(mesh_handle.index, expected_index);
            assert_eq!(material.index, expected_index);
            engine.meshes[mesh_handle.index as usize].generation = u32::MAX;
            engine.materials[material.index as usize].generation = u32::MAX;
            mesh_handle.generation = u32::MAX;
            material.generation = u32::MAX;
            engine.delete_mesh(mesh_handle).unwrap();
            engine.delete_material(material).unwrap();
        }
        assert!(engine.create_mesh(mesh(1.0)).is_err());
        assert!(engine.create_material(Material::default()).is_err());
        assert!(engine.live_mesh_handles().next().is_none());
    }

    #[test]
    fn failed_mutations_do_not_consume_reusable_slots_or_replace_live_data() {
        let mut engine = Engine::new(capacity(1, 1)).unwrap();
        let material = engine.create_material(Material::default()).unwrap();
        engine.delete_material(material).unwrap();
        assert!(
            engine
                .create_material(Material {
                    tint: [f32::NAN; 4]
                })
                .is_err()
        );
        let current_material = engine.create_material(Material::default()).unwrap();
        assert_eq!(current_material.index, material.index);
        assert_eq!(current_material.generation, material.generation + 1);
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
        let other = Engine::new(capacity(1, 1)).unwrap();
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
    fn snapshots_keep_old_mesh_and_material_and_compose_transforms() {
        let pool = SnapshotPool::new(1, 2).unwrap();
        let mut engine = Engine::new(capacity(1, 1)).unwrap();
        let mesh_handle = engine.create_mesh(mesh(1.0)).unwrap();
        let material = engine.create_material(Material::default()).unwrap();
        let mut camera = view();
        camera.clip_from_world[0][0] = 2.0;
        let mut model = IDENTITY;
        model[3][0] = 0.25;
        let old = engine
            .snapshot(
                &pool,
                &camera,
                &[Draw {
                    mesh: mesh_handle,
                    material,
                    world_from_model: model,
                }],
            )
            .unwrap();
        assert_eq!(old.draws[0].parameters.transform()[3][0], 0.5);
        engine.update_mesh(mesh_handle, mesh(0.0)).unwrap();
        engine
            .update_material(material, Material { tint: [0.5; 4] })
            .unwrap();
        let new = engine
            .snapshot(
                &pool,
                &view(),
                &[Draw {
                    mesh: mesh_handle,
                    material,
                    world_from_model: IDENTITY,
                }],
            )
            .unwrap();
        assert_eq!(old.draws[0].revision, 1);
        assert_eq!(new.draws[0].revision, 2);
        assert_eq!(old.draws[0].parameters.tint(), &[1.0; 4]);
        assert_eq!(new.draws[0].parameters.tint(), &[0.5; 4]);
        assert_ne!(old.draws[0].data.vertices, new.draws[0].data.vertices);
        engine.delete_mesh(mesh_handle).unwrap();
        engine.delete_material(material).unwrap();
        assert_eq!(old.draws[0].data.index_count, 3);
        assert_eq!(old.draws[0].data.indices.len(), 12);
    }

    #[test]
    fn invalid_input_does_not_replace_live_data() {
        let pool = SnapshotPool::new(1, 1).unwrap();
        assert!(MeshData::new(&[], &[]).is_err());
        let vertex = Vertex {
            position: [0.0; 3],
            color: [1.0; 4],
        };
        assert!(MeshData::new(&[vertex], &[0, 0, 1]).is_err());
        assert!(MeshData::new(&[vertex], &[0, 0]).is_err());
        assert!(
            MeshData::new(
                &[Vertex {
                    position: [f32::NAN; 3],
                    ..vertex
                }],
                &[0, 0, 0]
            )
            .is_err()
        );
        let mut engine = Engine::new(capacity(1, 1)).unwrap();
        let material = engine.create_material(Material::default()).unwrap();
        assert!(
            engine
                .update_material(
                    material,
                    Material {
                        tint: [f32::NAN; 4]
                    }
                )
                .is_err()
        );
        let handle = engine.create_mesh(mesh(1.0)).unwrap();
        let draw = Draw {
            mesh: handle,
            material,
            world_from_model: IDENTITY,
        };
        assert_eq!(
            engine.snapshot(&pool, &view(), &[draw]).unwrap().draws[0]
                .parameters
                .tint(),
            &[1.0; 4]
        );
        let mut bad = view();
        bad.clip_from_world[0][0] = f32::INFINITY;
        assert!(engine.snapshot(&pool, &bad, &[draw]).is_err());
        bad.clip_from_world = IDENTITY;
        bad.clip_from_world[0][0] = f32::MAX;
        let mut overflowing = draw;
        overflowing.world_from_model[0][0] = f32::MAX;
        assert!(engine.snapshot(&pool, &bad, &[overflowing]).is_err());
        assert_eq!(
            engine.snapshot(&pool, &view(), &[]).unwrap().draw_count(),
            0
        );
    }

    #[test]
    fn snapshot_slots_reuse_capacity_and_return_after_partial_failure() {
        let pool = SnapshotPool::new(2, 1).unwrap();
        let mut engine = Engine::new(capacity(1, 1)).unwrap();
        let mesh = engine.create_mesh(mesh(1.0)).unwrap();
        let material = engine.create_material(Material::default()).unwrap();
        let draw = Draw {
            mesh,
            material,
            world_from_model: IDENTITY,
        };
        let frame = engine.snapshot(&pool, &view(), &[draw, draw]).unwrap();
        let storage = frame.draws.as_ptr();
        assert_eq!(
            engine.snapshot(&pool, &view(), &[]).unwrap_err().kind(),
            EngineErrorKind::CapacityExhausted,
        );
        drop(frame);
        let foreign = Draw {
            material: MaterialHandle {
                owner: 0,
                ..material
            },
            ..draw
        };
        assert!(engine.snapshot(&pool, &view(), &[draw, foreign]).is_err());
        let frame = engine.snapshot(&pool, &view(), &[draw]).unwrap();
        assert_eq!(frame.draws.as_ptr(), storage);
        assert_eq!(frame.draw_count(), 1);
        drop(frame);
        assert_eq!(
            engine
                .snapshot(&pool, &view(), &[draw; 3])
                .unwrap_err()
                .kind(),
            EngineErrorKind::CapacityExhausted,
        );
        assert_eq!(
            engine.snapshot(&pool, &view(), &[]).unwrap().draw_count(),
            0
        );
    }

    #[test]
    fn retired_meshes_wait_for_snapshot_and_both_upload_streams() {
        let pool = SnapshotPool::new(1, 1).unwrap();
        let mut engine = Engine::new(EngineCapacity {
            meshes: 1,
            materials: 1,
            retired_mesh_versions: 1,
        })
        .unwrap();
        let handle = engine.create_mesh(mesh(1.0)).unwrap();
        let material = engine.create_material(Material::default()).unwrap();
        let frame = engine
            .snapshot(
                &pool,
                &view(),
                &[Draw {
                    mesh: handle,
                    material,
                    world_from_model: IDENTITY,
                }],
            )
            .unwrap();
        let vertices = Arc::clone(&frame.draws[0].data.vertices);
        let indices = Arc::clone(&frame.draws[0].data.indices);
        engine.update_mesh(handle, mesh(0.5)).unwrap();
        assert_eq!(engine.collect_retired_meshes(), 0);
        assert_eq!(
            engine.update_mesh(handle, mesh(0.0)).unwrap_err().kind(),
            EngineErrorKind::CapacityExhausted,
        );
        assert_eq!(engine.mesh_revision(handle).unwrap(), 2);
        assert_eq!(
            engine.delete_mesh(handle).unwrap_err().kind(),
            EngineErrorKind::CapacityExhausted,
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
        let mut engine = Engine::new(EngineCapacity {
            meshes: 1,
            materials: 1,
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

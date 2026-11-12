//! Small, GPU-independent resource and frame-input contracts.
//!
//! The caller owns scene semantics. This module only validates mesh/material
//! handles and captures immutable data needed to build a frame's graph.

use std::{
    collections::BTreeSet,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::graph::RasterParameters;

mod graph;
pub use graph::FrameGraph;

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
        if indices
            .iter()
            .any(|&index| index as usize >= vertices.len())
        {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineError {
    pub message: String,
}

impl EngineError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
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
}

impl Engine {
    pub fn new(mesh_capacity: usize, material_capacity: usize) -> Result<Self, EngineError> {
        if mesh_capacity == 0
            || material_capacity == 0
            || mesh_capacity > u32::MAX as usize
            || material_capacity > u32::MAX as usize
        {
            return Err(EngineError::new(
                "asset capacities must be nonzero and fit u32",
            ));
        }
        let id = NEXT_ENGINE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| EngineError::new("engine identity exhausted"))?;
        Ok(Self {
            id,
            meshes: Vec::new(),
            materials: Vec::new(),
            free_meshes: BTreeSet::new(),
            free_materials: BTreeSet::new(),
            mesh_capacity,
            material_capacity,
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
        *mesh = MeshVersion {
            revision,
            data: Arc::new(data),
        };
        Ok(())
    }
    pub fn delete_mesh(&mut self, handle: MeshHandle) -> Result<(), EngineError> {
        self.check_owner(handle.owner)?;
        lookup(&self.meshes, handle.index, handle.generation)?;
        self.meshes[handle.index as usize].value = None;
        if handle.generation < u32::MAX {
            self.free_meshes.insert(handle.index as usize);
        }
        Ok(())
    }
    pub fn mesh_revision(&self, handle: MeshHandle) -> Result<u64, EngineError> {
        self.check_owner(handle.owner)?;
        Ok(lookup(&self.meshes, handle.index, handle.generation)?.revision)
    }
    pub fn live_mesh_handles(&self) -> Vec<MeshHandle> {
        self.meshes
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.value.as_ref().map(|_| MeshHandle {
                    owner: self.id,
                    index: index as u32,
                    generation: slot.generation,
                })
            })
            .collect()
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
    pub fn snapshot(&self, view: &View, draws: &[Draw]) -> Result<FrameSnapshot, EngineError> {
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
        let mut captured = Vec::with_capacity(draws.len());
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
            if !finite_matrix(transform) {
                return Err(EngineError::new("combined clip transform is not finite"));
            }
            captured.push(SnapshotDraw {
                mesh: draw.mesh,
                revision: mesh.revision,
                data: Arc::clone(&mesh.data),
                parameters: RasterParameters {
                    transform,
                    tint: material.tint,
                },
            });
        }
        Ok(FrameSnapshot {
            owner: self.id,
            view: *view,
            draws: captured,
        })
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
#[derive(Clone, Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(
        dead_code,
        reason = "captured frame operands are consumed by the optional Vulkan bridge"
    )
)]
pub struct FrameSnapshot {
    pub(crate) owner: u64,
    pub(crate) view: View,
    pub(crate) draws: Vec<SnapshotDraw>,
}
impl FrameSnapshot {
    pub fn view(&self) -> &View {
        &self.view
    }
    pub fn draw_count(&self) -> usize {
        self.draws.len()
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
        return Err(EngineError::new("asset capacity exhausted"));
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
        let mut first = Engine::new(1, 1).unwrap();
        let second = Engine::new(1, 1).unwrap();
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
        let mut engine = Engine::new(COUNT, COUNT).unwrap();
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
        assert_eq!(engine.live_mesh_handles().len(), COUNT);
        assert!(engine.create_mesh(data).is_err());
        assert!(engine.create_material(Material::default()).is_err());
    }

    #[test]
    fn generation_exhausted_slots_are_retired_instead_of_reused() {
        let mut engine = Engine::new(2, 2).unwrap();
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
        assert!(engine.live_mesh_handles().is_empty());
    }

    #[test]
    fn failed_mutations_do_not_consume_reusable_slots_or_replace_live_data() {
        let mut engine = Engine::new(1, 1).unwrap();
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
        let other = Engine::new(1, 1).unwrap();
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
        let mut engine = Engine::new(1, 1).unwrap();
        let mesh_handle = engine.create_mesh(mesh(1.0)).unwrap();
        let material = engine.create_material(Material::default()).unwrap();
        let mut camera = view();
        camera.clip_from_world[0][0] = 2.0;
        let mut model = IDENTITY;
        model[3][0] = 0.25;
        let old = engine
            .snapshot(
                &camera,
                &[Draw {
                    mesh: mesh_handle,
                    material,
                    world_from_model: model,
                }],
            )
            .unwrap();
        assert_eq!(old.draws[0].parameters.transform[3][0], 0.5);
        engine.update_mesh(mesh_handle, mesh(0.0)).unwrap();
        engine
            .update_material(material, Material { tint: [0.5; 4] })
            .unwrap();
        let new = engine
            .snapshot(
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
        assert_eq!(old.draws[0].parameters.tint, [1.0; 4]);
        assert_eq!(new.draws[0].parameters.tint, [0.5; 4]);
        assert_ne!(old.draws[0].data.vertices, new.draws[0].data.vertices);
        engine.delete_mesh(mesh_handle).unwrap();
        engine.delete_material(material).unwrap();
        assert_eq!(old.draws[0].data.index_count, 3);
        assert_eq!(old.draws[0].data.indices.len(), 12);
    }

    #[test]
    fn invalid_input_does_not_replace_live_data() {
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
        let mut engine = Engine::new(1, 1).unwrap();
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
            engine.snapshot(&view(), &[draw]).unwrap().draws[0]
                .parameters
                .tint,
            [1.0; 4]
        );
        let mut bad = view();
        bad.clip_from_world[0][0] = f32::INFINITY;
        assert!(engine.snapshot(&bad, &[draw]).is_err());
        bad.clip_from_world = IDENTITY;
        bad.clip_from_world[0][0] = f32::MAX;
        let mut overflowing = draw;
        overflowing.world_from_model[0][0] = f32::MAX;
        assert!(engine.snapshot(&bad, &[overflowing]).is_err());
        assert_eq!(engine.snapshot(&view(), &[]).unwrap().draw_count(), 0);
    }
}

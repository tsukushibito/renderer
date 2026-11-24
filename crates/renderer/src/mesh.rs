//! Registered, indexed mesh primitives for the view pipeline.
use crate::{
    pbr::{Bounds, PbrVertex},
    pipeline::Mesh,
    unlit_forward,
};
pub use renderer_core::geometry::{MeshCapacity, MeshHandle};
use renderer_core::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    geometry::{GeometryData, MeshError, MeshRegistry},
    graph::{BufferDesc, Graph, GraphError, ImportBuffer, ResourceKey},
    program::{ManagedRasterProgram, VertexAttribute},
};
use std::{collections::HashMap, sync::Arc};
use upload::UploadBatch;
mod skin;
mod upload;
pub(crate) use skin::FramePoses;
pub use skin::{JointWeights, SkinPoseId};
pub(crate) use upload::MeshCheckpoint;
pub use upload::MeshUploadProgress;

const VERTEX_BUFFER: &str = "mesh vertices";
const INDEX_BUFFER: &str = "mesh indices";

/// One indexed primitive and the vertex interpretation established at import.
pub struct MeshData {
    geometry: GeometryData,
    bounds: Bounds,
    attributes: Vec<VertexAttribute>,
    skin: Option<Arc<skin::Skin>>,
}
impl MeshData {
    pub fn pbr(vertices: &[PbrVertex], indices: &[u32]) -> Result<Self, GraphError> {
        let program = crate::passes::pbr::opaque_program();
        let bytes: Vec<_> = vertices.iter().flat_map(|v| v.encode()).collect();
        let geometry = GeometryData::new(program.vertex_stride(), &bytes, indices)?;
        Ok(Self::custom(
            geometry,
            indexed_bounds(indices, |i| vertices[i].position())?,
            program.descriptor().vertex_attributes,
        ))
    }
    pub fn with_lightmap_uvs(mut self, uvs: &[[f32; 2]]) -> Result<Self, GraphError> {
        if uvs.iter().flatten().any(|v| !(0.0..=1.0).contains(v)) {
            return Err(GraphError::invalid_input(
                "lightmap UV outside the unit rectangle",
            ));
        }
        let bytes = uvs
            .iter()
            .flatten()
            .copied()
            .flat_map(f32::to_le_bytes)
            .collect();
        self.geometry = self.geometry.with_secondary_vertices(8, bytes)?;
        Ok(self)
    }
    pub fn unlit(vertices: &[unlit_forward::Vertex], indices: &[u32]) -> Result<Self, GraphError> {
        let geometry = unlit_forward::mesh_data(vertices, indices)
            .map_err(|e| GraphError::invalid_input(e.to_string()))?;
        Ok(Self::custom(
            geometry,
            indexed_bounds(indices, |i| vertices[i].position)?,
            ManagedRasterProgram::mesh_hdr()
                .descriptor()
                .vertex_attributes,
        ))
    }
    pub fn custom(geometry: GeometryData, bounds: Bounds, attributes: &[VertexAttribute]) -> Self {
        Self {
            geometry,
            bounds,
            attributes: attributes.to_vec(),
            skin: None,
        }
    }
}

fn indexed_bounds(
    indices: &[u32],
    point: impl Fn(usize) -> [f32; 3],
) -> Result<Bounds, GraphError> {
    // GeometryData has already checked that the index stream is nonempty and in range.
    let mut min = point(indices[0] as usize);
    let mut max = min;
    for &index in &indices[1..] {
        let p = point(index as usize);
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    Bounds::new(min, max)
}
struct Layout {
    position_at_start: bool,
    skin: Option<Arc<skin::Skin>>,
    bounds: Bounds,
    attributes: Vec<VertexAttribute>,
    vertex_stride: u32,
    index_count: u32,
}
impl MeshData {
    fn into_parts(self) -> (GeometryData, Arc<Layout>) {
        let layout = Layout {
            position_at_start: self.attributes.iter().any(|a| {
                a.location == 0
                    && a.offset == 0
                    && a.format == renderer_core::program::VertexFormat::Float32x3
            }),
            skin: self.skin,
            bounds: self.bounds,
            attributes: self.attributes,
            vertex_stride: self.geometry.vertex_stride(),
            index_count: self.geometry.index_count(),
        };
        (self.geometry, Arc::new(layout))
    }
}
struct Resident {
    layout: Arc<Layout>,
    revision: u64,
    buffers: [Option<ImportBuffer>; 3],
}
#[derive(Default)]
struct Upload {
    keys: [Option<ResourceKey>; 3],
    accepted_bytes: u64,
}
// Published layout ownership moves here until explicit GPU collection. Import
// tokens return their range slots at publication; only their registrations remain.
struct RetiredVersion {
    keys: [Option<ResourceKey>; 3],
    _layout: Option<Arc<Layout>>,
}

struct Asset {
    selected_epoch: u64,
    layout: Arc<Layout>,
    revision: u64,
    active: Option<Resident>,
    upload: Upload,
}
impl Asset {
    fn drawable_layout(&self) -> &Layout {
        self.active
            .as_ref()
            .map_or(&self.layout, |ready| &ready.layout)
    }

    fn retire_upload(&mut self, retired: &mut Vec<RetiredVersion>) {
        let keys = std::mem::take(&mut self.upload).keys;
        if keys.iter().any(Option::is_some) {
            retired.push(RetiredVersion {
                keys,
                _layout: None,
            });
        }
    }
    fn retire_active(&mut self, retired: &mut Vec<RetiredVersion>) {
        if let Some(active) = self.active.take() {
            retired.push(RetiredVersion {
                keys: active.buffers.map(|buffer| buffer.map(|b| b.key())),
                _layout: Some(active.layout),
            });
        }
    }
}

/// Owns mesh identities, CPU geometry and GPU registrations for one runtime.
pub struct Meshes {
    poses: HashMap<SkinPoseId, skin::Pose>,
    registry: MeshRegistry,
    assets: HashMap<MeshHandle, Asset>,
    retired: Vec<RetiredVersion>,
    batch: UploadBatch,
}
impl Meshes {
    pub(crate) fn geometry(&self, mesh: MeshHandle) -> Result<&GeometryData, GraphError> {
        self.registry
            .geometry(mesh)
            .map_err(|e| GraphError::invalid_input(e.to_string()))
    }

    /// Budget the two initialized imports for each distinct mesh referenced by
    /// this frame. Register their resident physical classes with [`Self::reserve`].
    pub fn reserve_imports(
        preparation: &mut Preparation,
        max_meshes: usize,
    ) -> Result<(), GraphError> {
        preparation.add_graph_requirements(
            GraphRequirements::imports(VERTEX_BUFFER, max_meshes, 1)?
                .checked_add(GraphRequirements::imports(INDEX_BUFFER, max_meshes, 1)?)?,
        )
    }
    pub fn new(capacity: MeshCapacity) -> Result<Self, MeshError> {
        let registry = MeshRegistry::new(capacity)?;
        // Between collections each live slot starts with at most a published
        // and an uploading version. Each accepted update/removal retires at
        // most two more. release_gpu drains the queue before detaching assets.
        let retired_capacity = capacity
            .meshes
            .checked_add(capacity.retired_mesh_versions)
            .and_then(|versions| versions.checked_mul(2))
            .ok_or_else(|| MeshError::capacity_exhausted("mesh retirement capacity overflow"))?;
        let mut retired = Vec::new();
        retired.try_reserve_exact(retired_capacity).map_err(|_| {
            MeshError::capacity_exhausted("unable to reserve mesh retirement queue")
        })?;
        let mut assets = HashMap::new();
        assets
            .try_reserve(capacity.meshes)
            .map_err(|_| MeshError::capacity_exhausted("unable to reserve mesh assets"))?;
        Ok(Self {
            poses: HashMap::new(),
            registry,
            assets,
            retired,
            batch: UploadBatch::new(capacity.meshes)?,
        })
    }
    pub fn create(&mut self, data: MeshData) -> Result<MeshHandle, MeshError> {
        let (geometry, layout) = data.into_parts();
        let handle = self.registry.create_mesh(geometry)?;
        self.assets.insert(
            handle,
            Asset {
                selected_epoch: 0,
                layout,
                revision: 1,
                active: None,
                upload: Upload::default(),
            },
        );
        Ok(handle)
    }
    /// Stage new geometry. The current drawable version remains available until
    /// the complete replacement is accepted by the GPU queue.
    pub fn update(&mut self, mesh: MeshHandle, data: MeshData) -> Result<(), MeshError> {
        let (geometry, layout) = data.into_parts();
        self.registry.update_mesh(mesh, geometry)?;
        let asset = self.assets.get_mut(&mesh).expect("registered mesh");
        asset.retire_upload(&mut self.retired);
        asset.layout = layout;
        asset.revision = self.registry.mesh_revision(mesh).expect("registered mesh");
        Ok(())
    }
    pub fn remove(&mut self, mesh: MeshHandle) -> Result<(), MeshError> {
        self.registry.delete_mesh(mesh)?;
        self.poses.retain(|_, pose| pose.mesh != mesh);
        let mut asset = self.assets.remove(&mesh).expect("registered mesh");
        asset.retire_upload(&mut self.retired);
        asset.retire_active(&mut self.retired);
        Ok(())
    }

    pub(crate) fn source_revision(&self, mesh: MeshHandle) -> Result<u64, MeshError> {
        self.registry.mesh_revision(mesh)
    }

    pub(crate) fn resident_revision(&self, mesh: MeshHandle) -> Option<u64> {
        Some(self.assets.get(&mesh)?.active.as_ref()?.revision)
    }

    pub(crate) fn retirement_usage(&self) -> (usize, usize) {
        self.registry.retirement_usage()
    }
    /// Register resident buffer classes once. Budget overlapping versions at updates.
    pub fn reserve(&self, preparation: &mut Preparation) -> Result<(), GraphError> {
        self.reserve_selection(preparation, self.registry.live_mesh_handles())
    }

    pub(crate) fn reserve_selection(
        &self,
        preparation: &mut Preparation,
        handles: impl Iterator<Item = MeshHandle>,
    ) -> Result<(), GraphError> {
        let mut classes = Vec::new();
        let mut upload = GraphRequirements::default();
        for handle in handles {
            let data = self.geometry(handle)?;
            let extra = usize::from(data.secondary_byte_len() > 0);
            upload = upload.checked_add(GraphRequirements {
                resources: 2 + extra,
                passes: 2 + extra,
                accesses: 2 + extra,
                name_bytes: "mesh verticesmesh indicesmesh vertex uploadmesh index upload".len()
                    + extra * "mesh UVmesh UV upload".len(),
                ..Default::default()
            })?;
            for size in [
                data.vertex_byte_len(),
                data.index_byte_len(),
                data.secondary_byte_len(),
            ]
            .into_iter()
            .filter(|&s| s > 0)
            {
                classes.push(ResourceClass::Buffer {
                    desc: BufferDesc { size },
                    count: 1,
                });
            }
        }
        preparation.add_requirements(
            upload,
            WorkingSet {
                resources: &classes,
                ..Default::default()
            },
        )
    }
    pub(crate) fn revision(&self, mesh: MeshHandle) -> Result<u64, GraphError> {
        let asset = self
            .assets
            .get(&mesh)
            .ok_or_else(|| GraphError::invalid_input("unknown mesh handle"))?;
        Ok(asset
            .active
            .as_ref()
            .map_or(asset.revision, |ready| ready.revision))
    }
    /// Reserve a spare vertex/index pair for staging a replacement of the given
    /// sizes while previous draws continue to reference the published version.
    pub fn reserve_replacement(
        &self,
        mesh: MeshHandle,
        preparation: &mut Preparation,
    ) -> Result<(), GraphError> {
        let data = self
            .registry
            .geometry(mesh)
            .map_err(|e| GraphError::invalid_input(e.to_string()))?;
        if data.secondary_byte_len() > 0 {
            preparation.reserve_buffer(
                BufferDesc {
                    size: data.secondary_byte_len(),
                },
                1,
            )?;
        }
        preparation.add_working_set(WorkingSet {
            resources: &[
                ResourceClass::Buffer {
                    desc: BufferDesc {
                        size: data.vertex_byte_len(),
                    },
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc {
                        size: data.index_byte_len(),
                    },
                    count: 1,
                },
            ],
            ..Default::default()
        })
    }
    pub(crate) fn position_stride(&self, mesh: MeshHandle) -> Result<u32, GraphError> {
        let layout = self
            .assets
            .get(&mesh)
            .ok_or_else(|| GraphError::invalid_input("unknown mesh"))?
            .drawable_layout();
        if !layout.position_at_start {
            return Err(GraphError::invalid_input(
                "picking requires a float3 position at location and offset zero",
            ));
        }
        Ok(layout.vertex_stride)
    }

    /// Check a mesh/program pairing when building or updating the scene.
    pub fn check_layout(
        &self,
        mesh: MeshHandle,
        program: &ManagedRasterProgram,
    ) -> Result<(), GraphError> {
        let asset = self
            .assets
            .get(&mesh)
            .ok_or_else(|| GraphError::invalid_input("unknown mesh handle"))?;
        let layout = asset.drawable_layout();
        if layout.attributes != program.descriptor().vertex_attributes
            || layout.vertex_stride != program.vertex_stride()
        {
            return Err(GraphError::invalid_input(
                "mesh vertex layout differs from material",
            ));
        }
        Ok(())
    }

    pub(crate) fn import_lightmap_uvs(
        &self,
        graph: &mut Graph,
        mesh: MeshHandle,
    ) -> Result<renderer_core::graph::BufferHandle, GraphError> {
        let stream = self
            .assets
            .get(&mesh)
            .and_then(|a| a.active.as_ref())
            .and_then(|a| a.buffers[2].as_ref())
            .ok_or_else(|| {
                GraphError::invalid_input(
                    "static placement requires a resident mesh with lightmap UVs",
                )
            })?;
        graph.import_readonly_buffer("mesh UV", stream)
    }
    /// Import the current resident vertex/index range into this Graph.
    pub fn import(&self, graph: &mut Graph, mesh: MeshHandle) -> Result<Mesh, GraphError> {
        self.import_geometry(graph, mesh, None)
    }

    pub(crate) fn import_geometry(
        &self,
        graph: &mut Graph,
        mesh: MeshHandle,
        posed: Option<skin::PosedStream>,
    ) -> Result<Mesh, GraphError> {
        let asset = self
            .assets
            .get(&mesh)
            .ok_or_else(|| GraphError::invalid_input("unknown mesh handle"))?;
        let active = asset
            .active
            .as_ref()
            .ok_or_else(|| GraphError::invalid_input("prepare the mesh before drawing"))?;
        let (vertices, bounds) = match posed {
            Some(stream) => (stream.vertices, stream.bounds),
            None => (
                graph.import_readonly_buffer(VERTEX_BUFFER, active.buffers[0].as_ref().unwrap())?,
                active.layout.bounds,
            ),
        };
        Ok(Mesh {
            bounds,
            vertex_stride: active.layout.vertex_stride,
            vertices,
            indices: graph
                .import_readonly_buffer(INDEX_BUFFER, active.buffers[1].as_ref().unwrap())?,
            first_index: 0,
            index_count: active.layout.index_count,
        })
    }
}

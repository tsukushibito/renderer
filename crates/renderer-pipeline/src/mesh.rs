//! Registered, indexed mesh primitives for the view pipeline.
use crate::{
    pbr::{Bounds, PbrVertex},
    pipeline::Mesh,
    unlit_forward,
};
pub use renderer::geometry::{MeshCapacity, MeshHandle};
use renderer::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    geometry::{GeometryData, MeshError, MeshRegistry},
    graph::{BufferDesc, Graph, GraphError, ImportBuffer},
    program::{ManagedRasterProgram, VertexAttribute},
};
use std::collections::HashMap;

/// One indexed primitive and the vertex interpretation established at import.
pub struct MeshData {
    geometry: GeometryData,
    bounds: Bounds,
    attributes: Vec<VertexAttribute>,
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
struct Asset {
    bounds: Bounds,
    attributes: Vec<VertexAttribute>,
    buffers: Option<[ImportBuffer; 2]>,
}

/// Owns the CPU registry and GPU registrations for one runtime.
pub struct Meshes {
    registry: MeshRegistry,
    assets: HashMap<MeshHandle, Asset>,
    retired: Vec<ImportBuffer>,
}
impl Meshes {
    pub fn new(capacity: MeshCapacity) -> Result<Self, MeshError> {
        Ok(Self {
            registry: MeshRegistry::new(capacity)?,
            assets: HashMap::new(),
            retired: Vec::new(),
        })
    }
    pub fn create(&mut self, data: MeshData) -> Result<MeshHandle, MeshError> {
        let handle = self.registry.create_mesh(data.geometry)?;
        self.assets.insert(
            handle,
            Asset {
                bounds: data.bounds,
                attributes: data.attributes,
                buffers: None,
            },
        );
        Ok(handle)
    }
    pub fn update(&mut self, mesh: MeshHandle, data: MeshData) -> Result<(), MeshError> {
        self.registry.update_mesh(mesh, data.geometry)?;
        let old = self
            .assets
            .insert(
                mesh,
                Asset {
                    bounds: data.bounds,
                    attributes: data.attributes,
                    buffers: None,
                },
            )
            .expect("registered mesh");
        if let Some(buffers) = old.buffers {
            self.retired.extend(buffers);
        }
        Ok(())
    }
    pub fn remove(&mut self, mesh: MeshHandle) -> Result<(), MeshError> {
        self.registry.delete_mesh(mesh)?;
        if let Some(buffers) = self.assets.remove(&mesh).and_then(|asset| asset.buffers) {
            self.retired.extend(buffers);
        }
        Ok(())
    }
    /// Register resident buffer classes once. Budget overlapping versions at updates.
    pub fn reserve(&self, preparation: &mut Preparation) -> Result<(), GraphError> {
        let mut classes = Vec::new();
        let mut upload = GraphRequirements::default();
        for handle in self.registry.live_mesh_handles() {
            let data = self.registry.geometry(handle).expect("live mesh");
            upload = upload.checked_add(GraphRequirements {
                resources: 2,
                passes: 2,
                accesses: 2,
                name_bytes: "mesh verticesmesh indicesmesh vertex uploadmesh index upload".len(),
                upload_bytes: usize::try_from(data.vertex_byte_len() + data.index_byte_len())
                    .map_err(|_| GraphError::invalid_input("mesh upload size overflow"))?,
                ..Default::default()
            })?;
            for size in [data.vertex_byte_len(), data.index_byte_len()] {
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
        self.registry
            .mesh_revision(mesh)
            .map_err(|e| GraphError::invalid_input(e.to_string()))
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
        let geometry = self.registry.geometry(mesh).expect("registered mesh");
        if asset.attributes != program.descriptor().vertex_attributes
            || geometry.vertex_stride() != program.vertex_stride()
        {
            return Err(GraphError::invalid_input(
                "mesh vertex layout differs from material",
            ));
        }
        Ok(())
    }

    /// Import the current resident vertex/index range into this Graph.
    pub fn import(&self, graph: &mut Graph, mesh: MeshHandle) -> Result<Mesh, GraphError> {
        let asset = self
            .assets
            .get(&mesh)
            .ok_or_else(|| GraphError::invalid_input("unknown mesh handle"))?;
        let geometry = self.registry.geometry(mesh).expect("registered mesh");
        let buffers = asset
            .buffers
            .as_ref()
            .ok_or_else(|| GraphError::invalid_input("prepare the mesh before drawing"))?;
        Ok(Mesh {
            bounds: asset.bounds,
            vertex_stride: geometry.vertex_stride(),
            vertices: graph.import_buffer("mesh vertices", &buffers[0])?,
            indices: graph.import_buffer("mesh indices", &buffers[1])?,
            first_index: 0,
            index_count: geometry.index_count(),
        })
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
impl Meshes {
    /// Upload all new or changed versions in one submission at an asset boundary.
    pub fn prepare_gpu(
        &mut self,
        runtime: &mut renderer::runtime::Runtime,
        pool: &mut renderer::graph::GraphPool,
    ) -> Result<Option<renderer::runtime::FrameId>, crate::pbr::DefaultResourceError> {
        if self.assets.values().all(|a| a.buffers.is_some()) {
            return Ok(None);
        }
        let mut graph = pool.begin()?;
        let mut pending = Vec::new();
        for (&mesh, asset) in &self.assets {
            if asset.buffers.is_some() {
                continue;
            }
            let data = self.registry.geometry(mesh).expect("registered mesh");
            let v = graph.buffer(
                "mesh vertices",
                BufferDesc {
                    size: data.vertex_byte_len(),
                },
            )?;
            let i = graph.buffer(
                "mesh indices",
                BufferDesc {
                    size: data.index_byte_len(),
                },
            )?;
            data.upload_vertices(&mut graph, "mesh vertex upload", v)?;
            data.upload_indices(&mut graph, "mesh index upload", i)?;
            graph.export_buffer(v)?;
            graph.export_buffer(i)?;
            pending.push((mesh, [v, i]));
        }
        let sent = runtime.submit(&graph.compile()?)?;
        for (mesh, handles) in pending {
            self.assets.get_mut(&mesh).expect("registered mesh").buffers =
                Some(handles.map(|h| sent.export_buffer(h).expect("accepted mesh export")));
        }
        Ok(Some(sent.id()))
    }
    /// Release obsolete versions after older Graphs are submitted or discarded.
    /// Accepted GPU work retains its leases.
    pub fn collect(
        &mut self,
        runtime: &mut renderer::runtime::Runtime,
    ) -> Result<(), renderer::runtime::ExecutionError> {
        while let Some(old) = self.retired.last() {
            runtime.delete(old.key())?;
            self.retired.pop();
        }
        self.registry.collect_retired_meshes();
        Ok(())
    }
    pub fn clear(
        &mut self,
        runtime: &mut renderer::runtime::Runtime,
    ) -> Result<(), renderer::runtime::ExecutionError> {
        for asset in self.assets.values_mut() {
            if let Some(buffers) = asset.buffers.take() {
                self.retired.extend(buffers);
            }
        }
        self.collect(runtime)
    }
}

//! Engine input bridge. Asset identity and draw construction are separate from
//! native resource keys, Graph lowering and asynchronous frame retirement.

use std::{collections::BTreeMap, fmt};

use super::graph::{FrameSubmission, RuntimeError, VulkanRuntime};
use crate::{
    engine::{
        Draw, Engine, EngineError, FrameSnapshot, Material, MaterialHandle, MeshData, MeshHandle,
        View,
    },
    graph::{
        BufferDesc, BufferHandle, CompiledGraph, Graph, GraphError, ImageHandle, ImportImage,
        PassHandle, ResourceKey,
    },
};

#[derive(Debug)]
pub enum EngineRenderError {
    Input(EngineError),
    Graph(GraphError),
    Runtime(RuntimeError),
}
impl fmt::Display for EngineRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(e) => e.fmt(f),
            Self::Graph(e) => e.fmt(f),
            Self::Runtime(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for EngineRenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Input(e) => e,
            Self::Graph(e) => e,
            Self::Runtime(e) => e,
        })
    }
}
impl From<EngineError> for EngineRenderError {
    fn from(e: EngineError) -> Self {
        Self::Input(e)
    }
}
impl From<GraphError> for EngineRenderError {
    fn from(e: GraphError) -> Self {
        Self::Graph(e)
    }
}
impl From<RuntimeError> for EngineRenderError {
    fn from(e: RuntimeError) -> Self {
        Self::Runtime(e)
    }
}

struct CachedMesh {
    revision: u64,
    vertices: ResourceKey,
    indices: ResourceKey,
}

/// Frame metadata. Dropping it does not wait for the GPU. `output` is registered
/// until the caller explicitly deletes its key after scheduling its consumers.
pub struct RenderedFrame {
    pub submission: FrameSubmission,
    pub output: ImportImage,
    pub readback: Option<PassHandle>,
}

/// An owned frame ready for extra ordinary Graph declarations and compilation.
/// Submission revalidates imported revisions. Building does not submit work or
/// register native resource names, and dropping this value does not wait.
pub struct PreparedEngineFrame {
    pub graph: Graph,
    pub output: ImageHandle,
    pub readback: Option<PassHandle>,
    owner: u64,
    source_graph: u64,
    source_output: ImageHandle,
    created: Vec<(MeshHandle, u64, BufferHandle, BufferHandle)>,
}

/// Minimal engine connection on top of the ordinary Graph and runtime APIs.
/// Persistent meshes upload once per version; materials and per-draw parameters
/// are copied into each frame's immutable operation operands.
pub struct VulkanEngine {
    runtime: VulkanRuntime,
    assets: Engine,
    meshes: BTreeMap<MeshHandle, CachedMesh>,
}

impl VulkanEngine {
    pub fn new(
        runtime: VulkanRuntime,
        mesh_capacity: usize,
        material_capacity: usize,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            runtime,
            assets: Engine::new(mesh_capacity, material_capacity)?,
            meshes: BTreeMap::new(),
        })
    }
    pub fn runtime(&self) -> &VulkanRuntime {
        &self.runtime
    }
    pub fn runtime_mut(&mut self) -> &mut VulkanRuntime {
        &mut self.runtime
    }
    pub fn assets(&self) -> &Engine {
        &self.assets
    }
    pub fn create_mesh(&mut self, mesh: MeshData) -> Result<MeshHandle, EngineError> {
        self.assets.create_mesh(mesh)
    }
    pub fn create_material(&mut self, material: Material) -> Result<MaterialHandle, EngineError> {
        self.assets.create_material(material)
    }
    pub fn update_material(
        &mut self,
        handle: MaterialHandle,
        material: Material,
    ) -> Result<(), EngineError> {
        self.assets.update_material(handle, material)
    }
    pub fn delete_material(&mut self, handle: MaterialHandle) -> Result<(), EngineError> {
        self.assets.delete_material(handle)
    }
    pub fn update_mesh(
        &mut self,
        handle: MeshHandle,
        mesh: MeshData,
    ) -> Result<(), EngineRenderError> {
        self.assets.update_mesh(handle, mesh)?;
        self.release_mesh(handle)
    }
    pub fn delete_mesh(&mut self, handle: MeshHandle) -> Result<(), EngineRenderError> {
        self.assets.delete_mesh(handle)?;
        self.release_mesh(handle)
    }
    fn release_mesh(&mut self, handle: MeshHandle) -> Result<(), EngineRenderError> {
        if let Some(mesh) = self.meshes.remove(&handle) {
            // Registration ends now; recorded and pending Graph leases retain
            // these buffers until actual retirement. No resource wait occurs.
            self.runtime.delete(mesh.vertices)?;
            self.runtime.delete(mesh.indices)?;
        }
        Ok(())
    }
    pub fn snapshot(&self, view: &View, draws: &[Draw]) -> Result<FrameSnapshot, EngineError> {
        self.assets.snapshot(view, draws)
    }
    pub fn render_frame(
        &mut self,
        view: &View,
        draws: &[Draw],
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<RenderedFrame, EngineRenderError> {
        let prepared = self.build_frame(view, draws, readback, extension)?;
        let plan = prepared.graph.compile()?;
        self.submit_frame(prepared, &plan)
    }
    pub fn build_frame(
        &self,
        view: &View,
        draws: &[Draw],
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<PreparedEngineFrame, EngineRenderError> {
        let snapshot = self.snapshot(view, draws)?;
        self.build_snapshot(&snapshot, readback, extension)
    }
    /// An owned snapshot can outlive CPU asset updates or deletion. Such old
    /// versions remain usable here, but are transient and do not replace the
    /// cache for the current asset version. Snapshots from another Engine fail.
    pub fn render_snapshot(
        &mut self,
        snapshot: &FrameSnapshot,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<RenderedFrame, EngineRenderError> {
        let prepared = self.build_snapshot(snapshot, readback, extension)?;
        let plan = prepared.graph.compile()?;
        self.submit_frame(prepared, &plan)
    }
    pub fn build_snapshot(
        &self,
        snapshot: &FrameSnapshot,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<PreparedEngineFrame, EngineRenderError> {
        if snapshot.owner != self.assets.id {
            return Err(EngineError::new("frame snapshot belongs to another Engine").into());
        }
        let mut graph = Graph::new();
        let mut resolved = BTreeMap::new();
        let mut created = Vec::new();
        for draw in &snapshot.draws {
            if resolved.contains_key(&draw.mesh) {
                continue;
            }
            let current = self.assets.mesh_revision(draw.mesh).ok() == Some(draw.revision);
            let cached = self
                .meshes
                .get(&draw.mesh)
                .filter(|mesh| current && mesh.revision == draw.revision);
            let pair = if let Some(mesh) = cached {
                // Read-only usage also advances scheduled revisions: keep keys,
                // never a cached ImportBuffer snapshot, between frame builds.
                (
                    graph.import_buffer(
                        "persistent mesh vertices",
                        &self.runtime.refresh_buffer(mesh.vertices)?,
                    )?,
                    graph.import_buffer(
                        "persistent mesh indices",
                        &self.runtime.refresh_buffer(mesh.indices)?,
                    )?,
                )
            } else {
                let vertices = graph.buffer(
                    "mesh vertices",
                    BufferDesc {
                        size: draw.data.vertices.len() as u64,
                    },
                )?;
                let indices = graph.buffer(
                    "mesh indices",
                    BufferDesc {
                        size: draw.data.indices.len() as u64,
                    },
                )?;
                graph.upload_buffer_shared(
                    "mesh vertex upload",
                    vertices,
                    0,
                    draw.data.vertices.clone(),
                )?;
                graph.upload_buffer_shared(
                    "mesh index upload",
                    indices,
                    0,
                    draw.data.indices.clone(),
                )?;
                if current {
                    graph.export_buffer(vertices)?;
                    graph.export_buffer(indices)?;
                    created.push((draw.mesh, draw.revision, vertices, indices));
                }
                (vertices, indices)
            };
            resolved.insert(draw.mesh, pair);
        }
        let frame = snapshot.build_graph_with_meshes(graph, &resolved, readback, extension)?;
        Ok(PreparedEngineFrame {
            source_graph: frame.graph.id,
            source_output: frame.output,
            graph: frame.graph,
            output: frame.output,
            readback: frame.readback,
            owner: self.assets.id,
            created,
        })
    }
    pub fn submit_frame(
        &mut self,
        prepared: PreparedEngineFrame,
        plan: &CompiledGraph,
    ) -> Result<RenderedFrame, EngineRenderError> {
        if prepared.owner != self.assets.id
            || prepared.source_graph != plan.id
            || prepared.graph.id != plan.id
            || prepared.output != prepared.source_output
            || !plan.exports.contains(&prepared.output.index)
            || prepared.created.iter().any(|(_, _, vertices, indices)| {
                !plan.exports.contains(&vertices.index) || !plan.exports.contains(&indices.index)
            })
        {
            return Err(EngineError::new(
                "prepared frame or compiled plan belongs to another owner/Graph",
            )
            .into());
        }
        let submission = self.runtime.submit_frame(plan)?;
        // Cache only accepted exports. If recording or submit fails, no stale
        // resource key is published as an initialized mesh version.
        for (handle, revision, vertices, indices) in prepared.created {
            let vertices = submission.export_buffer(vertices)?.key();
            let indices = submission.export_buffer(indices)?.key();
            if self.assets.mesh_revision(handle).ok() == Some(revision) {
                // A second prepared frame may upload the same version. Retire
                // the previous cache registration before replacing its keys.
                self.release_mesh(handle)?;
                self.meshes.insert(
                    handle,
                    CachedMesh {
                        revision,
                        vertices,
                        indices,
                    },
                );
            } else {
                self.runtime.delete(vertices)?;
                self.runtime.delete(indices)?;
            }
        }
        let output = submission.export_image(prepared.output)?;
        Ok(RenderedFrame {
            submission,
            output,
            readback: prepared.readback,
        })
    }
}

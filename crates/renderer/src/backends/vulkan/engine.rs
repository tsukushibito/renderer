//! Backend residency and explicit upload progress for backend-independent input.

use super::graph::{FrameSubmission, RuntimeError, Submission, VulkanRuntime};
use crate::{
    engine::{EngineError, FrameOutputOwnership, FrameSnapshot, MeshData, MeshHandle},
    graph::{CompiledGraph, Graph, GraphError, ImageHandle, ImportImage, PassHandle, ResourceKey},
};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};
mod residency;
use residency::{MeshVersionKey, ResidentBuffers, ResidentMesh};

/// Logical registered mesh bounds; not total VRAM or pending-native limits.
#[derive(Clone, Copy, Debug)]
pub struct MeshCacheConfig {
    pub max_meshes: usize,
    pub max_bytes: u64,
    pub max_upload_submissions: usize,
}
impl Default for MeshCacheConfig {
    fn default() -> Self {
        Self {
            max_meshes: 16,
            max_bytes: 64 * 1024 * 1024,
            max_upload_submissions: 1,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MeshPreparationError {
    WorkingSetTooLarge {
        required_meshes: usize,
        required_bytes: u64,
        max_meshes: usize,
        max_bytes: u64,
    },
    ResourceCapacityTooSmall {
        required_resources: usize,
        capacity: usize,
    },
    UploadBusy,
    ResidencyPinned,
    NotResident,
}
impl fmt::Display for MeshPreparationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkingSetTooLarge {
                required_meshes,
                required_bytes,
                max_meshes,
                max_bytes,
            } => write!(
                f,
                "mesh working set requires {required_meshes} meshes/{required_bytes} bytes; residency permits {max_meshes}/{max_bytes}"
            ),
            Self::ResourceCapacityTooSmall {
                required_resources,
                capacity,
            } => write!(
                f,
                "snapshot needs at least {required_resources} physical resources including view targets; runtime permits {capacity}"
            ),
            Self::UploadBusy => {
                f.write_str("another mesh working set is uploading; finish or explicitly cancel it")
            }
            Self::ResidencyPinned => {
                f.write_str("prepared frames retain required residency; submit or drop them")
            }
            Self::NotResident => f.write_str(
                "snapshot meshes are not resident; prepare and advance upload progress first",
            ),
        }
    }
}
impl std::error::Error for MeshPreparationError {}
#[derive(Debug)]
pub enum EngineRenderError {
    Input(EngineError),
    Graph(GraphError),
    Runtime(RuntimeError),
    Preparation(MeshPreparationError),
}
impl fmt::Display for EngineRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(e) => e.fmt(f),
            Self::Graph(e) => e.fmt(f),
            Self::Runtime(e) => e.fmt(f),
            Self::Preparation(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for EngineRenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Input(e) => e,
            Self::Graph(e) => e,
            Self::Runtime(e) => e,
            Self::Preparation(e) => e,
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
impl From<MeshPreparationError> for EngineRenderError {
    fn from(e: MeshPreparationError) -> Self {
        Self::Preparation(e)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshUploadState {
    Ready,
    Submitted,
    Backpressure,
}
/// Ready means all required bytes were accepted, not CPU Fence completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshUploadProgress {
    pub state: MeshUploadState,
    pub submitted_bytes: u64,
    pub remaining_bytes: u64,
    pub pending_submissions: usize,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MeshCacheStats {
    pub resident_meshes: usize,
    pub resident_logical_bytes: u64,
    pub ready_meshes: usize,
    pub queued_meshes: usize,
    pub prepared_retained_meshes: usize,
    pub pending_upload_submissions: usize,
    pub evictions: usize,
}
/// Output ownership authority is independent of replaceable public metadata.
pub struct RenderedFrame {
    pub submission: FrameSubmission,
    pub output: ImportImage,
    pub readback: Option<PassHandle>,
    output_ownership: FrameOutputOwnership,
    owned_output_key: Option<ResourceKey>,
}
impl RenderedFrame {
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.output_ownership
    }
}
/// Pins registered immutable mesh versions until submission or drop.
pub struct PreparedEngineFrame {
    pub graph: Graph,
    pub output: ImageHandle,
    pub readback: Option<PassHandle>,
    owner: Arc<()>,
    source_graph: u64,
    source_output: ImageHandle,
    output_ownership: FrameOutputOwnership,
    pins: Vec<Arc<ResidentBuffers>>,
}
impl PreparedEngineFrame {
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.output_ownership
    }
}
/// GPU residency consumes immutable snapshots from any CPU Engine; no asset registry.
pub struct VulkanEngine {
    runtime: VulkanRuntime,
    config: MeshCacheConfig,
    owner: Arc<()>,
    residents: BTreeMap<MeshVersionKey, ResidentMesh>,
    active_upload: Option<Vec<MeshVersionKey>>,
    pending_uploads: Vec<Submission>,
    clock: AtomicU64,
    evictions: usize,
}
impl VulkanEngine {
    pub fn new(runtime: VulkanRuntime, config: MeshCacheConfig) -> Result<Self, EngineError> {
        if config.max_meshes == 0
            || config.max_meshes > u32::MAX as usize
            || config.max_bytes == 0
            || config.max_upload_submissions == 0
        {
            return Err(EngineError::new(
                "mesh residency and upload bounds must be nonzero; mesh count must fit u32",
            ));
        }
        let mut pending_uploads = Vec::new();
        pending_uploads
            .try_reserve_exact(config.max_upload_submissions)
            .map_err(|_| EngineError::new("unable to reserve bounded upload bookkeeping"))?;
        Ok(Self {
            runtime,
            config,
            owner: Arc::new(()),
            residents: BTreeMap::new(),
            active_upload: None,
            pending_uploads,
            clock: AtomicU64::new(0),
            evictions: 0,
        })
    }
    pub fn runtime(&self) -> &VulkanRuntime {
        &self.runtime
    }
    pub fn runtime_mut(&mut self) -> &mut VulkanRuntime {
        &mut self.runtime
    }
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
    /// Construct only: missing residency is reported without hidden uploads.
    pub fn build_snapshot(
        &self,
        snapshot: &FrameSnapshot,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<PreparedEngineFrame, EngineRenderError> {
        let mut graph = Graph::new();
        let mut resolved = BTreeMap::new();
        let mut pins = Vec::new();
        let serial = self.next_use()?;
        for draw in &snapshot.draws {
            if resolved.contains_key(&draw.mesh) {
                continue;
            }
            let resident = self
                .residents
                .get(&MeshVersionKey::new(draw.mesh, draw.revision))
                .filter(|entry| entry.ready() && !entry.retiring)
                .ok_or(MeshPreparationError::NotResident)?;
            let buffers = resident.buffers.as_ref().expect("ready mesh owns buffers");
            // Content revisions stay stable for reads; native scopes rebase at submit.
            let vertices = graph.import_buffer(
                "resident mesh vertices",
                &self.runtime.refresh_buffer(buffers.vertices)?,
            )?;
            let indices = graph.import_buffer(
                "resident mesh indices",
                &self.runtime.refresh_buffer(buffers.indices)?,
            )?;
            resident
                .last_use
                .store(serial, std::sync::atomic::Ordering::Relaxed);
            resolved.insert(draw.mesh, (vertices, indices));
            pins.push(Arc::clone(buffers));
        }
        let frame = snapshot.build_graph_with_meshes(graph, &resolved, readback, extension)?;
        let output_ownership = frame.output_ownership();
        Ok(PreparedEngineFrame {
            source_graph: frame.graph.id,
            source_output: frame.output,
            graph: frame.graph,
            output: frame.output,
            readback: frame.readback,
            owner: Arc::clone(&self.owner),
            output_ownership,
            pins,
        })
    }
    pub fn submit_frame(
        &mut self,
        prepared: PreparedEngineFrame,
        plan: &CompiledGraph,
    ) -> Result<RenderedFrame, EngineRenderError> {
        if !Arc::ptr_eq(&prepared.owner, &self.owner)
            || prepared.source_graph != plan.id
            || prepared.graph.id != plan.id
            || prepared.output != prepared.source_output
            || !plan.has_export(prepared.output.index)
        {
            return Err(EngineError::new(
                "prepared frame or compiled plan belongs to another owner/Graph",
            )
            .into());
        }
        let submission = self.runtime.submit_frame(plan)?;
        let output = submission.export_image(prepared.output)?;
        let ownership = prepared.output_ownership;
        drop(prepared.pins);
        self.reap_retiring()?;
        Ok(RenderedFrame {
            submission,
            owned_output_key: (ownership == FrameOutputOwnership::Frame).then_some(output.key()),
            output,
            readback: prepared.readback,
            output_ownership: ownership,
        })
    }
    /// Release the original frame-created registration exactly once; imported outputs stay caller-owned.
    pub fn release_output(&mut self, frame: &mut RenderedFrame) -> Result<(), EngineRenderError> {
        if let Some(key) = frame.owned_output_key {
            self.runtime.delete(key)?;
            frame.owned_output_key = None;
        }
        Ok(())
    }
}

//! Backend residency and explicit upload progress for backend-independent input.

use super::graph::{FrameSubmission, RuntimeError, Submission, VulkanRuntime};
use crate::{
    engine::{EngineError, FrameOutputOwnership, FrameSnapshot, MeshData, MeshHandle},
    graph::{
        CompiledGraph, Graph, GraphCapacity, GraphError, GraphPool, ImageHandle, ImportImage,
        PassHandle, ResourceKey,
    },
};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};
mod residency;
use residency::{ActiveUpload, DeferredSources, MeshVersionKey, ResidentBuffers, ResidentMesh};
mod frames;
use frames::{FrameScratchLease, FrameScratchPool};

/// CPU construction bounds, independent of native execution and CPU results.
#[derive(Clone, Copy, Debug)]
pub struct EngineFrameCapacity {
    /// CPU prepared/compiled frames retaining resident-mesh pins.
    pub prepared_frames: usize,
    /// Graphs held by CPU frames, accepted work or retained content provenance.
    pub graph_slots: usize,
    pub graph: GraphCapacity,
}

impl Default for EngineFrameCapacity {
    fn default() -> Self {
        Self {
            prepared_frames: 4,
            graph_slots: 8,
            graph: GraphCapacity::default(),
        }
    }
}

/// Logical registered mesh bounds; not total VRAM or pending-native limits.
#[derive(Clone, Copy, Debug)]
pub struct MeshCacheConfig {
    pub max_meshes: usize,
    pub max_bytes: u64,
    pub max_upload_submissions: usize,
    /// CPU source versions retained after GPU residency ends. This is separate
    /// from `max_meshes`; each version was admitted within `max_bytes`.
    pub max_retired_sources: usize,
}
impl Default for MeshCacheConfig {
    fn default() -> Self {
        Self {
            max_meshes: 16,
            max_bytes: 64 * 1024 * 1024,
            max_upload_submissions: 1,
            max_retired_sources: 16,
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
    FrameCapacityExhausted,
    SourceRetirementCapacityExhausted,
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
            Self::FrameCapacityExhausted => f.write_str(
                "prepared frame capacity exhausted; submit or drop a prepared frame before retrying",
            ),
            Self::SourceRetirementCapacityExhausted => f.write_str(
                "retired mesh source capacity exhausted; collect with the CPU asset owner or after source consumers finish",
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
    pub retired_sources: usize,
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
    scratch: FrameScratchLease,
}
impl PreparedEngineFrame {
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.output_ownership
    }

    /// Consume this mutable graph while retaining the original output authority
    /// and resident mesh pins until submission or cancellation.
    pub fn compile(self) -> Result<CompiledEngineFrame, GraphError> {
        Ok(CompiledEngineFrame {
            graph: self.graph.compile()?,
            output: self.output,
            readback: self.readback,
            owner: self.owner,
            source_graph: self.source_graph,
            source_output: self.source_output,
            output_ownership: self.output_ownership,
            scratch: self.scratch,
        })
    }
}

pub struct CompiledEngineFrame {
    pub graph: CompiledGraph,
    pub output: ImageHandle,
    pub readback: Option<PassHandle>,
    owner: Arc<()>,
    source_graph: u64,
    source_output: ImageHandle,
    output_ownership: FrameOutputOwnership,
    scratch: FrameScratchLease,
}

impl CompiledEngineFrame {
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.output_ownership
    }
}
/// GPU residency consumes immutable snapshots from any CPU Engine; no asset registry.
pub struct VulkanEngine {
    runtime: VulkanRuntime,
    config: MeshCacheConfig,
    owner: Arc<()>,
    graph_pool: GraphPool,
    upload_graphs: GraphPool,
    frame_scratch: FrameScratchPool,
    residents: BTreeMap<MeshVersionKey, ResidentMesh>,
    retired_sources: DeferredSources,
    active_upload: Option<ActiveUpload>,
    idle_upload_keys: Vec<MeshVersionKey>,
    required_meshes: Vec<(MeshVersionKey, Arc<MeshData>)>,
    pending_uploads: Vec<Submission>,
    clock: AtomicU64,
    evictions: usize,
}
impl VulkanEngine {
    pub fn new(
        runtime: VulkanRuntime,
        config: MeshCacheConfig,
        frames: EngineFrameCapacity,
    ) -> Result<Self, EngineError> {
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
        let mut idle_upload_keys = Vec::new();
        let mut required_meshes = Vec::new();
        idle_upload_keys
            .try_reserve_exact(config.max_meshes)
            .map_err(|_| EngineError::new("unable to reserve bounded mesh upload keys"))?;
        required_meshes
            .try_reserve_exact(config.max_meshes)
            .map_err(|_| EngineError::new("unable to reserve bounded mesh admission scratch"))?;
        let graph_pool = GraphPool::new(frames.graph, frames.graph_slots)
            .map_err(|_| EngineError::new("unable to reserve Engine Graph storage"))?;
        let upload_graphs = GraphPool::new(frames.graph, config.max_upload_submissions)
            .map_err(|_| EngineError::new("unable to reserve mesh upload Graph storage"))?;
        let frame_scratch = FrameScratchPool::new(config.max_meshes, frames.prepared_frames)?;
        Ok(Self {
            runtime,
            config,
            owner: Arc::new(()),
            graph_pool,
            upload_graphs,
            frame_scratch,
            residents: BTreeMap::new(),
            retired_sources: DeferredSources::new(config.max_retired_sources)?,
            active_upload: None,
            idle_upload_keys,
            required_meshes,
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

    /// Release asset inputs retained by idle Graph backing at an explicit
    /// asset-change or maintenance boundary. Call this before collecting retired
    /// CPU mesh versions. Recorded, GPU and canonical content owners stay pinned.
    pub fn release_idle_inputs(&mut self) -> usize {
        self.graph_pool.release_idle() + self.upload_graphs.release_idle()
    }

    pub fn render_snapshot(
        &mut self,
        snapshot: &FrameSnapshot,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<RenderedFrame, EngineRenderError> {
        let prepared = self.build_snapshot(snapshot, readback, extension)?;
        self.submit_frame(prepared.compile()?)
    }
    /// Construct only: missing residency is reported without hidden uploads.
    pub fn build_snapshot(
        &mut self,
        snapshot: &FrameSnapshot,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<PreparedEngineFrame, EngineRenderError> {
        let mut scratch = self.frame_scratch.acquire()?;
        let mut graph = self.graph_pool.begin()?;
        let frame_scratch = scratch.scratch();
        let serial = self.next_use()?;
        for draw in &snapshot.draws {
            let Err(index) = frame_scratch
                .resolved
                .binary_search_by_key(&draw.mesh, |(mesh, _)| *mesh)
            else {
                continue;
            };
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
            // The ready resident set is bounded by max_meshes; these arrays
            // were reserved for that bound at Engine construction.
            frame_scratch
                .resolved
                .insert(index, (draw.mesh, (vertices, indices)));
            frame_scratch.pins.push(Arc::clone(buffers));
        }
        let frame = snapshot.build_graph_with_meshes(
            graph,
            |mesh| {
                frame_scratch
                    .resolved
                    .binary_search_by_key(&mesh, |(mesh, _)| *mesh)
                    .ok()
                    .map(|index| frame_scratch.resolved[index].1)
            },
            readback,
            extension,
        )?;
        let output_ownership = frame.output_ownership();
        Ok(PreparedEngineFrame {
            source_graph: frame.graph.id,
            source_output: frame.output,
            graph: frame.graph,
            output: frame.output,
            readback: frame.readback,
            owner: Arc::clone(&self.owner),
            output_ownership,
            scratch,
        })
    }
    pub fn submit_frame(
        &mut self,
        prepared: CompiledEngineFrame,
    ) -> Result<RenderedFrame, EngineRenderError> {
        if !Arc::ptr_eq(&prepared.owner, &self.owner)
            || prepared.source_graph != prepared.graph.id
            || prepared.output != prepared.source_output
            || !prepared.graph.has_export(prepared.output.index)
        {
            return Err(EngineError::new(
                "prepared frame or compiled plan belongs to another owner/Graph",
            )
            .into());
        }
        let submission = self.runtime.submit_frame(&prepared.graph)?;
        let output = submission.export_image(prepared.output)?;
        let ownership = prepared.output_ownership;
        drop(prepared.scratch);
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

#[cfg(test)]
mod workload_tests;

#[cfg(test)]
pub(crate) mod test_input;

#[cfg(all(test, feature = "memory-profiling"))]
mod retired_memory_tests;

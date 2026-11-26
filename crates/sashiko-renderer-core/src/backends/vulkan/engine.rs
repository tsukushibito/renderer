//! Backend residency and explicit upload progress for backend-independent input.

use super::graph::{FrameSubmission, RuntimeError, Submission, VulkanRuntime};
use crate::{
    geometry::{GeometryData, MeshError, MeshHandle, MeshSnapshot, SnapshotInputs},
    graph::{
        CompiledGraph, FrameGraph, FrameOutputOwnership, Graph, GraphCapacity, GraphError,
        GraphPool, ImageHandle, ImportImage, PassHandle, ResourceKey,
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
    Input(MeshError),
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
impl From<MeshError> for EngineRenderError {
    fn from(e: MeshError) -> Self {
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
/// A submitted frame's selected output and associated readback.
/// The engine releases a frame-created registration on explicit request.
pub struct RenderedFrame {
    submission: FrameSubmission,
    output: ImportImage,
    readback: Option<PassHandle>,
    output_ownership: FrameOutputOwnership,
    owned_output_key: Option<ResourceKey>,
}
impl RenderedFrame {
    pub fn submission(&self) -> &FrameSubmission {
        &self.submission
    }
    pub fn output(&self) -> &ImportImage {
        &self.output
    }
    pub fn readback(&self) -> Option<PassHandle> {
        self.readback
    }
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.output_ownership
    }
}
/// A caller-composed Graph with registered mesh versions pinned from connection
/// through compile and submission/cancellation. No output or drawing is implied.
pub struct PreparedEngineGraph {
    pub graph: Graph,
    owner: Arc<()>,
    source_graph: u64,
    scratch: FrameScratchLease,
}

impl PreparedEngineGraph {
    /// Optionally finish a single-image convenience frame after composition.
    /// Any supported extent/format is accepted. General multiple-image/buffer
    /// exports use Graph's export methods and submit_graph instead.
    pub fn select_output(
        self,
        output: ImageHandle,
        readback: bool,
    ) -> Result<PreparedEngineFrame, GraphError> {
        if self.source_graph != self.graph.id {
            return Err(GraphError::new(
                crate::graph::GraphErrorKind::WrongOwner,
                "replaced prepared Graph",
            ));
        }
        let frame = FrameGraph::select_output(
            self.graph,
            output,
            readback.then_some("frame output readback"),
        )?;
        Ok(PreparedEngineFrame {
            frame,
            owner: self.owner,
            scratch: self.scratch,
        })
    }

    pub fn compile(self) -> Result<CompiledEngineGraph, GraphError> {
        if self.source_graph != self.graph.id {
            return Err(GraphError::new(
                crate::graph::GraphErrorKind::WrongOwner,
                "replaced prepared Graph",
            ));
        }
        Ok(CompiledEngineGraph {
            graph: self.graph.compile()?,
            owner: self.owner,
            scratch: self.scratch,
        })
    }
}

/// The compiled plan is immutable and its resident-input pins remain attached.
pub struct CompiledEngineGraph {
    graph: CompiledGraph,
    owner: Arc<()>,
    scratch: FrameScratchLease,
}

impl CompiledEngineGraph {
    pub fn pass_count(&self) -> usize {
        self.graph.pass_count()
    }
    pub fn ordered_pass_names(&self) -> impl Iterator<Item = &str> {
        self.graph.ordered_pass_names()
    }
}

/// Holds the finalized output selection and pins its mesh versions through compilation.
///
/// ```compile_fail
/// use sashiko_renderer_core::vulkan::PreparedEngineFrame;
/// fn clear_readback(frame: &mut PreparedEngineFrame) {
///     frame.readback = None;
/// }
/// ```
pub struct PreparedEngineFrame {
    frame: FrameGraph,
    owner: Arc<()>,
    scratch: FrameScratchLease,
}

impl PreparedEngineFrame {
    pub fn graph(&self) -> &Graph {
        self.frame.graph()
    }
    pub fn output(&self) -> ImageHandle {
        self.frame.output()
    }
    pub fn readback(&self) -> Option<PassHandle> {
        self.frame.readback()
    }
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.frame.output_ownership()
    }

    pub fn compile(self) -> Result<CompiledEngineFrame, GraphError> {
        Ok(CompiledEngineFrame {
            output: self.frame.output(),
            readback: self.frame.readback(),
            output_ownership: self.frame.output_ownership(),
            graph: self.frame.compile()?,
            owner: self.owner,
            scratch: self.scratch,
        })
    }
}

/// A compiled frame carries its original graph, mesh pins and output selection together.
///
/// ```compile_fail
/// use sashiko_renderer_core::{graph::PassHandle, vulkan::CompiledEngineFrame};
/// fn redirect_readback(frame: &mut CompiledEngineFrame, unrelated: PassHandle) {
///     frame.readback = Some(unrelated);
/// }
/// ```
pub struct CompiledEngineFrame {
    graph: CompiledGraph,
    output: ImageHandle,
    readback: Option<PassHandle>,
    owner: Arc<()>,
    output_ownership: FrameOutputOwnership,
    scratch: FrameScratchLease,
}

impl CompiledEngineFrame {
    pub fn graph(&self) -> &CompiledGraph {
        &self.graph
    }
    pub fn output(&self) -> ImageHandle {
        self.output
    }
    pub fn readback(&self) -> Option<PassHandle> {
        self.readback
    }
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.output_ownership
    }
}
/// GPU residency consumes immutable snapshots from any CPU MeshRegistry; no asset registry.
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
    required_meshes: Vec<(MeshVersionKey, Arc<GeometryData>)>,
    pending_uploads: Vec<Submission>,
    clock: AtomicU64,
    evictions: usize,
}
impl VulkanEngine {
    pub fn new(
        runtime: VulkanRuntime,
        config: MeshCacheConfig,
        frames: EngineFrameCapacity,
    ) -> Result<Self, MeshError> {
        if config.max_meshes == 0
            || config.max_meshes > u32::MAX as usize
            || config.max_bytes == 0
            || config.max_upload_submissions == 0
        {
            return Err(MeshError::invalid_input(
                "mesh residency and upload bounds must be nonzero; mesh count must fit u32",
            ));
        }
        let mut pending_uploads = Vec::new();
        pending_uploads
            .try_reserve_exact(config.max_upload_submissions)
            .map_err(|_| {
                MeshError::invalid_input("unable to reserve bounded upload bookkeeping")
            })?;
        let mut idle_upload_keys = Vec::new();
        let mut required_meshes = Vec::new();
        idle_upload_keys
            .try_reserve_exact(config.max_meshes)
            .map_err(|_| MeshError::invalid_input("unable to reserve bounded mesh upload keys"))?;
        required_meshes
            .try_reserve_exact(config.max_meshes)
            .map_err(|_| {
                MeshError::invalid_input("unable to reserve bounded mesh admission scratch")
            })?;
        let graph_pool = GraphPool::new(frames.graph, frames.graph_slots).map_err(|_| {
            MeshError::invalid_input("unable to reserve MeshRegistry Graph storage")
        })?;
        let upload_graphs = GraphPool::new(frames.graph, config.max_upload_submissions)
            .map_err(|_| MeshError::invalid_input("unable to reserve mesh upload Graph storage"))?;
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

    /// Start a caller-composed Graph, including snapshot-free Compute/copy work.
    pub fn begin_graph(&mut self) -> Result<PreparedEngineGraph, EngineRenderError> {
        let scratch = self.frame_scratch.acquire()?;
        let graph = self.graph_pool.begin()?;
        Ok(PreparedEngineGraph {
            source_graph: graph.id,
            graph,
            owner: Arc::clone(&self.owner),
            scratch,
        })
    }

    /// Connect resident immutable inputs only. No uploads, waits, Raster,
    /// output images, exports or readback are added. A missing version reports
    /// NotResident. The returned construction lease does not own the native pins.
    pub fn connect_snapshot<'a>(
        &mut self,
        frame: &mut PreparedEngineGraph,
        snapshot: &'a MeshSnapshot,
    ) -> Result<SnapshotInputs<'a>, EngineRenderError> {
        if !Arc::ptr_eq(&frame.owner, &self.owner) || frame.source_graph != frame.graph.id {
            return Err(
                MeshError::invalid_input("prepared Graph belongs to another owner/Graph").into(),
            );
        }
        self.connect_inputs(&mut frame.graph, &mut frame.scratch, snapshot)
    }

    fn connect_inputs<'a>(
        &mut self,
        graph: &mut Graph,
        scratch: &mut FrameScratchLease,
        snapshot: &'a MeshSnapshot,
    ) -> Result<SnapshotInputs<'a>, EngineRenderError> {
        let serial = self.next_use()?;
        // Resolve and pin before exposing logical handles to user code. Keys
        // include revision because one Graph may read old and new snapshots.
        let frame_scratch = scratch.scratch();
        snapshot.connect_with(graph, |graph, draw| {
            let key = MeshVersionKey::new(draw.mesh, draw.revision);
            let index = match frame_scratch
                .resolved
                .binary_search_by_key(&key, |(key, _)| *key)
            {
                Ok(index) => return Ok(frame_scratch.resolved[index].1),
                Err(index) => index,
            };
            if frame_scratch.resolved.len() == self.config.max_meshes {
                return Err(MeshPreparationError::FrameCapacityExhausted.into());
            }
            let resident = self
                .residents
                .get(&key)
                .filter(|entry| entry.ready() && !entry.retiring)
                .ok_or(MeshPreparationError::NotResident)?;
            let buffers = resident.buffers.as_ref().expect("ready mesh owns buffers");
            let first_resource = graph.resources.len();
            let vertices = graph.import_buffer(
                "resident mesh vertices",
                &self.runtime.refresh_buffer(buffers.vertices)?,
            )?;
            let indices = graph.import_buffer(
                "resident mesh indices",
                &self.runtime.refresh_buffer(buffers.indices)?,
            )?;
            graph.protect_input(vertices, vertices.index >= first_resource)?;
            graph.protect_input(indices, indices.index >= first_resource)?;
            resident
                .last_use
                .store(serial, std::sync::atomic::Ordering::Relaxed);
            frame_scratch
                .resolved
                .insert(index, (key, (vertices, indices)));
            frame_scratch.pins.push(Arc::clone(buffers));
            Ok((vertices, indices))
        })
    }

    /// Submit any selected exports/readbacks. FrameSubmission owns the metadata;
    /// the caller releases each frame-created export registration with delete.
    /// Imported outputs retain their original ownership. Runtime retains native
    /// work independently through GPU completion and CPU result consumption.
    pub fn submit_graph(
        &mut self,
        frame: CompiledEngineGraph,
    ) -> Result<FrameSubmission, EngineRenderError> {
        if !Arc::ptr_eq(&frame.owner, &self.owner) {
            return Err(MeshError::invalid_input("compiled Graph belongs to another owner").into());
        }
        let submission = self.runtime.submit_frame(&frame.graph)?;
        drop(frame.scratch);
        Ok(submission)
    }

    pub fn submit_frame(
        &mut self,
        prepared: CompiledEngineFrame,
    ) -> Result<RenderedFrame, EngineRenderError> {
        if !Arc::ptr_eq(&prepared.owner, &self.owner) {
            return Err(
                MeshError::invalid_input("compiled frame belongs to another engine").into(),
            );
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

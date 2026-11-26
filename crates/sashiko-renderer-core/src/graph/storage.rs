//! Prepared backing shared by the unique builder and its immutable plans.

use super::{
    AttachmentOperand, BindingOperands, Dependency, Graph, GraphCapacityKind, GraphError, Pass,
    RasterBatchOperands, RasterConstants, RasterDrawOperands, Resource, ResourceAccess,
    ResourceKey, analysis,
};
use std::{
    cell::UnsafeCell,
    collections::HashMap,
    fmt,
    ops::{Deref, Range},
    sync::Arc,
};

/// Limits reserved at an explicit preparation boundary. Normal frames never grow
/// these buffers; changing a limit requires preparing a new pool.
///
/// Limits apply to each slot, not to the pool as a whole. Budget for the largest
/// declaration set, including branches that compilation will cull. Shared
/// bindings save operand storage, but each using pass still needs accesses.
/// Builder scratch can require more accesses than the final deduplicated plan.
/// The analysis limits describe metadata, not GPU memory or native capacities.
///
/// A failed builder returns its unpublished storage. Compilation consumes the
/// graph even on failure. Inspect [`GraphError::capacity`] to identify the bound:
/// return retained owners for slot backpressure, or prepare a new pool with the
/// relevant metadata bounds at a change boundary. No limit silently grows or
/// waits for GPU work. Analysis may report no exact `required` count, so tune
/// against representative maximum inputs, not only a single frame's draw count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphCapacity {
    /// Logical buffer/image declarations, including distinct canonical imports.
    /// Reimporting the same consistent snapshot does not consume another entry.
    pub resources: usize,
    /// Operation declarations, including passes that may later be culled.
    pub passes: usize,
    /// Encoded resource accesses and per-pass overlap/deduplication scratch.
    /// Include temporary duplicate reads before Raster access compression.
    pub accesses: usize,
    /// Draw operands across all Raster passes. Attachment-only passes consume
    /// no draw entries.
    pub draws: usize,
    /// Total inline constant words across all Raster batches. `None` reserves
    /// 32 words per draw. `Some(0)` permits only draws with empty constants.
    pub raster_constant_words: Option<usize>,
    /// Descriptor operands across Compute dispatches and distinct Raster binding
    /// sets. Reusing one set in more draws does not duplicate its operands.
    pub bindings: usize,
    /// Color plus depth attachment operands across Raster/attachment passes.
    /// Reusing a View in another pass consumes another operand.
    pub attachments: usize,
    /// Distinct explicit [`Graph::depends_on`] edges. Repeating the same pair
    /// does not consume capacity; inferred dependencies use `edges` instead.
    pub dependencies: usize,
    /// Initialized intervals captured by buffer/image imports across resources.
    /// This is separate from the backing slots of [`super::ImportRangesPool`].
    pub initial_ranges: usize,
    /// UTF-8 bytes in declaration names, including names of culled declarations.
    /// This counts bytes, not Unicode characters or a per-name maximum.
    pub name_bytes: usize,
    /// Total bytes captured by public buffer/image uploads in one graph.
    /// Compilation shares this storage rather than copying the payload again.
    pub upload_bytes: usize,
    /// Bound for each analysis edge arena: producer/dataflow and execution
    /// ordering. They are separate arenas with this same bound, not a combined
    /// edge count; fan-out and explicit dependencies can exceed the pass count.
    pub edges: usize,
    /// Bound for each resource-interval arena and for final initialized ranges.
    /// Partial writes/initial ranges split intervals, so resource count alone
    /// does not determine this requirement.
    pub segments: usize,
    /// Read-only access records retained by hazard analysis. One record is
    /// needed per live read-only access, independent of interval fragmentation.
    /// Development validation also analyzes otherwise culled declarations.
    pub readers: usize,
}
impl Default for GraphCapacity {
    fn default() -> Self {
        Self {
            resources: 256,
            passes: 256,
            accesses: 2048,
            draws: 2048,
            raster_constant_words: None,
            bindings: 2048,
            attachments: 512,
            dependencies: 2048,
            initial_ranges: 2048,
            name_bytes: 16_384,
            upload_bytes: 1_048_576,
            edges: 8192,
            segments: 4096,
            readers: 8192,
        }
    }
}
impl GraphCapacity {
    pub(super) fn analysis_capacity_error(self, kind: analysis::Capacity) -> GraphError {
        let (kind, limit) = match kind {
            analysis::Capacity::Resources => (GraphCapacityKind::Resources, self.resources),
            analysis::Capacity::Passes => (GraphCapacityKind::Passes, self.passes),
            analysis::Capacity::Accesses => (GraphCapacityKind::Accesses, self.accesses),
            analysis::Capacity::Edges => (GraphCapacityKind::Edges, self.edges),
            analysis::Capacity::Segments => (GraphCapacityKind::Segments, self.segments),
            analysis::Capacity::Readers => (GraphCapacityKind::Readers, self.readers),
        };
        capacity_error(kind, limit, None)
    }
    fn analysis_error(self, error: analysis::Error) -> GraphError {
        match error.kind {
            analysis::ErrorKind::Capacity(kind) => self.analysis_capacity_error(kind),
            _ => unreachable!("analysis preparation only fails on reservation"),
        }
    }
    fn analysis(self) -> analysis::Limits {
        analysis::Limits {
            resources: self.resources,
            passes: self.passes,
            accesses: self.accesses,
            edges: self.edges,
            segments: self.segments,
            readers: self.readers,
        }
    }
}
pub(super) fn capacity_error(
    kind: GraphCapacityKind,
    limit: usize,
    required: Option<usize>,
) -> GraphError {
    GraphError::capacity_error(kind, limit, required)
}
fn buffer<T>(capacity: usize, kind: GraphCapacityKind) -> Result<Vec<T>, GraphError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(capacity)
        .map_err(|_| capacity_error(kind, capacity, Some(capacity)))?;
    Ok(result)
}

/// Wall-clock time inside indexed capture and pass resource aggregation.
#[derive(Clone, Copy, Debug, Default)]
pub struct RasterBuildMetrics {
    /// Streaming input production, operand capture and per-draw read registration.
    pub streaming_capture: std::time::Duration,
    /// Generated operand and constant writes, including job dispatch and join.
    pub generated_capture: std::time::Duration,
    /// Ordered reduction of the workers' resource footprints.
    pub generated_merge: std::time::Duration,
    /// Vertex/index read aggregation at the end of all raster passes.
    pub finalize_reads: std::time::Duration,
    pub generated_runs: usize,
    pub generated_draws: usize,
}

/// Opaque prepared backing. Its fields and all mutable access are private to
/// Renderer; dereferencing a Graph or plan only exposes an immutable value.
#[doc(hidden)]
pub struct GraphStorage {
    pub(super) raster_metrics: Option<RasterBuildMetrics>,
    pub(crate) id: u64,
    pub(crate) resources: Vec<Resource>,
    pub(crate) passes: Vec<Pass>,
    pub(crate) exports: Vec<usize>,
    pub(crate) frame_image: Option<super::ImageHandle>,
    pub(super) exported: Vec<bool>,
    // One entry per imported resource; reserved for the complete resource limit.
    // Resource indices still follow caller order, never hash-table iteration.
    pub(super) imports: HashMap<ResourceKey, usize>,
    pub(super) accesses: Vec<ResourceAccess>,
    pub(super) dependencies: Vec<Dependency>,
    pub(super) initial_ranges: Vec<Range<u64>>,
    pub(super) names: Vec<u8>,
    pub(super) uploads: Vec<u8>,
    pub(super) draws: Vec<RasterDrawOperands>,
    pub(super) raster_batches: Vec<RasterBatchOperands>,
    pub(super) raster_constant_words: Vec<u32>,
    pub(super) raster_constant_word_limit: usize,
    pub(super) bindings: Vec<BindingOperands>,
    pub(super) binding_accesses: Vec<ResourceAccess>,
    pub(super) raster_binding_seen: Vec<bool>,
    pub(super) raster_binding_access_ranges: Vec<Range<usize>>,
    pub(super) raster_binding_sets: Vec<usize>,
    pub(super) attachments: Vec<AttachmentOperand>,
    pub(super) raster_ranges: Vec<[u64; 2]>,
    pub(super) raster_resources: Vec<usize>,
    pub(super) raster_index_ranges: Vec<(usize, Range<u64>)>,
    pub(super) overlap: Vec<super::accesses::IndexedAccess>,
    pub(super) read_accesses: super::accesses::ReadAccessSet,
    pub(super) workspace: analysis::Workspace,
    pub(super) output: analysis::Output,
    pub(super) capacity: GraphCapacity,
    #[cfg(feature = "graph-diagnostics")]
    pub(super) dump: std::sync::OnceLock<String>,
}
impl Deref for GraphStorage {
    type Target = analysis::Output;
    fn deref(&self) -> &Self::Target {
        &self.output
    }
}
#[cfg_attr(
    not(any(feature = "vulkan", all(feature = "d3d12", windows))),
    allow(dead_code, reason = "native plan readers are optional")
)]
impl GraphStorage {
    fn new(capacity: GraphCapacity) -> Result<Self, GraphError> {
        let raster_constant_word_limit = match capacity.raster_constant_words {
            Some(limit) => limit,
            None => capacity
                .draws
                .checked_mul(RasterConstants::MAX_WORDS)
                .ok_or_else(|| {
                    capacity_error(
                        GraphCapacityKind::RasterConstantWords,
                        isize::MAX as usize / size_of::<u32>(),
                        None,
                    )
                })?,
        };
        let mut raster_ranges = buffer(capacity.resources, GraphCapacityKind::Resources)?;
        raster_ranges.resize(capacity.resources, [0; 2]);
        let mut imports = HashMap::new();
        imports.try_reserve(capacity.resources).map_err(|_| {
            capacity_error(
                GraphCapacityKind::Resources,
                capacity.resources,
                Some(capacity.resources),
            )
        })?;
        let mut raster_binding_seen = buffer(capacity.bindings, GraphCapacityKind::Bindings)?;
        raster_binding_seen.resize(capacity.bindings, false);
        let mut raster_binding_access_ranges =
            buffer(capacity.bindings, GraphCapacityKind::Bindings)?;
        raster_binding_access_ranges.resize(capacity.bindings, 0..0);
        Ok(Self {
            id: 0,
            raster_metrics: None,
            resources: buffer(capacity.resources, GraphCapacityKind::Resources)?,
            passes: buffer(capacity.passes, GraphCapacityKind::Passes)?,
            exports: buffer(capacity.resources, GraphCapacityKind::Resources)?,
            frame_image: None,
            exported: buffer(capacity.resources, GraphCapacityKind::Resources)?,
            imports,
            accesses: buffer(capacity.accesses, GraphCapacityKind::Accesses)?,
            dependencies: buffer(capacity.dependencies, GraphCapacityKind::Dependencies)?,
            initial_ranges: buffer(capacity.initial_ranges, GraphCapacityKind::InitialRanges)?,
            names: buffer(capacity.name_bytes, GraphCapacityKind::NameBytes)?,
            uploads: buffer(capacity.upload_bytes, GraphCapacityKind::UploadBytes)?,
            draws: buffer(capacity.draws, GraphCapacityKind::Draws)?,
            // An encoded Program batch contains at least one draw.
            raster_batches: buffer(capacity.draws, GraphCapacityKind::Draws)?,
            raster_constant_words: buffer(
                raster_constant_word_limit,
                GraphCapacityKind::RasterConstantWords,
            )?,
            raster_constant_word_limit,
            bindings: buffer(capacity.bindings, GraphCapacityKind::Bindings)?,
            binding_accesses: buffer(capacity.bindings, GraphCapacityKind::Bindings)?,
            raster_binding_seen,
            raster_binding_access_ranges,
            raster_binding_sets: buffer(capacity.bindings, GraphCapacityKind::Bindings)?,
            attachments: buffer(capacity.attachments, GraphCapacityKind::Attachments)?,
            raster_ranges,
            raster_resources: buffer(capacity.resources, GraphCapacityKind::Resources)?,
            // Only nonzero first indices need a separate range per draw.
            raster_index_ranges: buffer(capacity.draws, GraphCapacityKind::Draws)?,
            overlap: buffer(capacity.accesses, GraphCapacityKind::Accesses)?,
            read_accesses: super::accesses::ReadAccessSet::new(
                capacity.accesses,
                capacity.resources,
            )
            .map_err(|kind| {
                let limit = match kind {
                    GraphCapacityKind::Resources => capacity.resources,
                    _ => capacity.accesses,
                };
                capacity_error(kind, limit, None)
            })?,
            workspace: analysis::Workspace::new(capacity.analysis())
                .map_err(|error| capacity.analysis_error(error))?,
            output: analysis::Output::new(capacity.analysis())
                .map_err(|error| capacity.analysis_error(error))?,
            capacity,
            #[cfg(feature = "graph-diagnostics")]
            dump: std::sync::OnceLock::new(),
        })
    }
    /// Measured phases for this checkout when the pool enabled raster profiling.
    pub fn raster_build_metrics(&self) -> Option<RasterBuildMetrics> {
        self.raster_metrics
    }

    fn reset(&mut self, id: u64) {
        self.clear_raster_ranges();
        self.id = id;
        self.passes.clear();
        self.resources.clear();
        self.exports.clear();
        self.frame_image = None;
        self.exported.clear();
        self.imports.clear();
        self.accesses.clear();
        self.dependencies.clear();
        self.initial_ranges.clear();
        self.names.clear();
        self.uploads.clear();
        self.draws.clear();
        self.raster_batches.clear();
        self.raster_constant_words.clear();
        self.bindings.clear();
        self.binding_accesses.clear();
        self.read_accesses.clear();
        self.attachments.clear();
        #[cfg(feature = "graph-diagnostics")]
        self.dump.take();
    }
    pub(super) fn clear_raster_ranges(&mut self) {
        for index in self.raster_binding_sets.drain(..) {
            self.raster_binding_seen[index] = false;
        }
        for &resource in &self.raster_resources {
            self.raster_ranges[resource] = [0; 2];
        }
        self.raster_resources.clear();
        self.raster_index_ranges.clear();
    }

    pub(super) fn track_raster_prefix(&mut self, resource: usize, purpose: usize, end: u64) {
        if self.raster_ranges[resource] == [0; 2] {
            // Each resource is added once, within the prepared resource limit.
            self.raster_resources.push(resource);
        }
        let previous = &mut self.raster_ranges[resource][purpose];
        *previous = (*previous).max(end);
    }
    pub(super) fn name(&self, range: &Range<usize>) -> &str {
        // Only UTF-8 source strings are appended, with boundaries retained.
        std::str::from_utf8(&self.names[range.clone()]).expect("stored Graph name is UTF-8")
    }
    pub(crate) fn resource_name(&self, resource: usize) -> &str {
        self.name(&self.resources[resource].name)
    }
    pub(crate) fn pass_name(&self, pass: usize) -> &str {
        self.name(&self.passes[pass].name)
    }
    #[cfg_attr(
        not(feature = "vulkan"),
        allow(dead_code, reason = "used by Vulkan lowering")
    )]
    pub(crate) fn accesses(&self, pass: usize) -> &[ResourceAccess] {
        &self.accesses[self.passes[pass].accesses.clone()]
    }
    #[cfg(all(
        feature = "development-validation",
        any(feature = "vulkan", all(feature = "d3d12", windows))
    ))]
    pub(crate) fn initial_contents(&self, resource: usize) -> &[Range<u64>] {
        &self.initial_ranges[self.resources[resource].initial.clone()]
    }
    #[cfg_attr(
        not(feature = "vulkan"),
        allow(dead_code, reason = "used by Vulkan lowering")
    )]
    pub(crate) fn final_contents(&self, resource: usize) -> &[Range<u64>] {
        self.output.final_contents(resource)
    }
    #[cfg_attr(
        not(feature = "vulkan"),
        allow(dead_code, reason = "used by Vulkan lowering")
    )]
    pub(crate) fn raster_draws(&self, range: &Range<usize>) -> &[RasterDrawOperands] {
        &self.draws[range.clone()]
    }
    pub(crate) fn raster_batches(&self, range: &Range<usize>) -> &[RasterBatchOperands] {
        &self.raster_batches[range.clone()]
    }
    pub(crate) fn raster_constants(
        &self,
        batch: &RasterBatchOperands,
    ) -> impl ExactSizeIterator<Item = &[u32]> {
        let stride = batch.constant_words_per_draw;
        let count = batch.draws.len();
        let mut words = if stride == 0 {
            &[]
        } else {
            // Construction appends exactly one ABI-sized payload per draw.
            &self.raster_constant_words[batch.constants_start..][..count * stride]
        };
        (0..count).map(move |_| {
            let (parameters, remaining) = words.split_at(stride);
            words = remaining;
            parameters
        })
    }
    #[cfg(sashiko_gpu_backend)]
    pub(crate) fn raster_bindings(&self, range: &Range<usize>) -> &[BindingOperands] {
        &self.bindings[range.clone()]
    }
    #[cfg(feature = "vulkan")]
    pub(crate) fn binding_count(&self) -> usize {
        self.bindings.len()
    }
    #[cfg(sashiko_gpu_backend)]
    pub(crate) fn raster_attachments(&self, range: &Range<usize>) -> &[AttachmentOperand] {
        &self.attachments[range.clone()]
    }
    #[cfg(sashiko_gpu_backend)]
    pub(crate) fn raster_attachment(&self, index: usize) -> &AttachmentOperand {
        &self.attachments[index]
    }
    pub(crate) fn upload_data<'a>(&'a self, operation: &'a super::Operation) -> &'a [u8] {
        let source = match operation {
            super::Operation::UploadBuffer { data, .. }
            | super::Operation::UploadImage { data, .. } => data,
            _ => unreachable!("upload_data requires an encoded upload operation"),
        };
        match source {
            super::UploadSource::Frame(range) => &self.uploads[range.clone()],
            super::UploadSource::Shared { bytes, source } => &bytes[source.clone()],
        }
    }
}
pub(super) struct Slot {
    pub(super) data: UnsafeCell<GraphStorage>,
}
// SAFETY: Only the non-Clone Graph can mutate a checked-out slot. begin requires
// Arc::get_mut before checkout, so every other builder, plan and native consumer
// has ended. Frozen plans only read, and no Weak or mutable backing escapes.
unsafe impl Sync for Slot {}
impl Slot {
    pub(super) fn data(&self) -> &GraphStorage {
        // SAFETY: The caller holds its builder/plan Arc; pool reuse is excluded.
        unsafe { &*self.data.get() }
    }
    pub(super) fn data_mut(&mut self) -> &mut GraphStorage {
        self.data.get_mut()
    }
}

/// Prepared Graph slots. Keep the pool alive through normal frame processing;
/// destroying it and its last plan is an explicit shutdown/change boundary.
pub struct GraphPool {
    raster_profiling: bool,
    generated_batch_size: Option<std::num::NonZeroUsize>,
    raster_jobs: Option<Arc<super::raster::CaptureJobs>>,
    slots: Vec<Arc<Slot>>,
    capacity: GraphCapacity,
}
impl fmt::Debug for GraphPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never inspect slot contents while a builder on another thread owns it.
        f.debug_struct("GraphPool")
            .field("slots", &self.slots.len())
            .field("capacity", &self.capacity)
            .finish()
    }
}
impl GraphPool {
    /// Reserve `slots` independent Graph backings with the given per-slot
    /// limits. This is a preparation boundary and can allocate. Zero slots and
    /// failed reservations return a classified capacity error.
    pub fn new(capacity: GraphCapacity, slots: usize) -> Result<Self, GraphError> {
        if slots == 0 {
            return Err(capacity_error(GraphCapacityKind::Slots, 0, Some(1)));
        }
        let mut prepared = buffer(slots, GraphCapacityKind::Slots)?;
        for _ in 0..slots {
            prepared.push(Arc::new(Slot {
                data: UnsafeCell::new(GraphStorage::new(capacity)?),
            }));
        }
        Ok(Self {
            slots: prepared,
            raster_profiling: false,
            generated_batch_size: None,
            raster_jobs: None,
            capacity,
        })
    }
    /// Use the supplied team to construct generated indexed runs at or above
    /// this draw count in future checkouts. This preparation can allocate.
    pub fn set_raster_jobs(
        &mut self,
        jobs: crate::jobs::SharedJobs,
        min_draws: std::num::NonZeroUsize,
    ) {
        self.raster_jobs = Some(Arc::new(super::raster::CaptureJobs::new(jobs, min_draws)));
    }
    /// Partition long generated runs into balanced, ordered native recording
    /// batches in future checkouts. Explicit streaming batches are kept intact.
    pub fn set_generated_batch_size(&mut self, max_draws: std::num::NonZeroUsize) {
        self.generated_batch_size = Some(max_draws);
    }

    /// Collect raster build durations at batch and pass boundaries on future
    /// checkouts. Disabled by default; ordinary capture reads no clocks.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub fn set_raster_profiling(&mut self, enabled: bool) {
        self.raster_profiling = enabled;
    }

    /// The immutable per-slot limits selected at preparation.
    pub fn capacity(&self) -> GraphCapacity {
        self.capacity
    }
    /// Observational count; use `is_idle` before replacing prepared backing.
    pub fn leased_slots(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| Arc::strong_count(slot) != 1)
            .count()
    }
    /// Establishes that no builder, plan or native consumer pins any slot.
    pub fn is_idle(&mut self) -> bool {
        self.slots
            .iter_mut()
            .all(|slot| Arc::get_mut(slot).is_some())
    }
    /// Explicit asset-change/maintenance operation. Release private asset pins
    /// from idle slots while retaining every prepared buffer. Active plans are
    /// untouched. Call before collecting retired asset versions; ordinary frame
    /// recycling does not require this maintenance operation.
    pub fn release_idle(&mut self) -> usize {
        let mut released = 0;
        for slot in &mut self.slots {
            if let Some(slot) = Arc::get_mut(slot) {
                let data = slot.data_mut();
                if data.id != 0 {
                    data.reset(0);
                    released += 1;
                }
            }
        }
        released
    }
    /// Borrow an idle prepared slot and give it a fresh handle identity.
    ///
    /// Returns `Slots` capacity backpressure while all slots are retained by
    /// builders, plans or native/CPU consumers; no allocation or wait is used.
    /// Dropping an unfinished builder or returning all pins of a compiled plan
    /// permits reuse. Handles from the previous checkout are always rejected.
    pub fn begin(&mut self) -> Result<Graph, GraphError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("renderer.graph.begin"), 0));
        for slot in &mut self.slots {
            if let Some(unique) = Arc::get_mut(slot) {
                let data = unique.data_mut();
                data.reset(super::next_id());
                data.raster_metrics = self.raster_profiling.then(RasterBuildMetrics::default);
                return Ok(Graph {
                    slot: Arc::clone(slot),
                    raster_jobs: self.raster_jobs.clone(),
                    generated_batch_size: self.generated_batch_size,
                });
            }
        }
        Err(capacity_error(
            GraphCapacityKind::Slots,
            self.slots.len(),
            self.slots.len().checked_add(1),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        Access, AccessKind, AccessPurpose, AccessRange, BufferDesc, GraphErrorKind, PassKind,
    };

    #[test]
    fn frozen_clones_pin_the_slot_and_reuse_rejects_old_handles() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let old = graph.buffer("old", BufferDesc { size: 4 }).unwrap();
        let plan = graph.compile().unwrap();
        let cloned = plan.clone();
        assert_eq!(
            pool.begin().unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
        drop(plan);
        assert_eq!(
            pool.begin().unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
        std::thread::spawn(move || drop(cloned)).join().unwrap();
        let mut next = pool.begin().unwrap();
        assert_eq!(
            next.export_buffer(old).unwrap_err().kind(),
            GraphErrorKind::WrongOwner
        );
        assert_eq!(next.compile().unwrap().pass_count(), 0);
    }

    #[test]
    fn cancelled_and_failed_builders_return_their_slots() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        drop(pool.begin().unwrap());
        let mut graph = pool.begin().unwrap();
        let bytes = graph.buffer("undefined", BufferDesc { size: 4 }).unwrap();
        graph.readback_buffer("read hole", bytes, 0, 4).unwrap();
        let error = graph.compile().unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
        assert_eq!(error.pass_name(), Some("read hole"));
        assert_eq!(error.resource_name(), Some("undefined"));
        assert_eq!(pool.begin().unwrap().compile().unwrap().pass_count(), 0);
    }

    #[test]
    fn failed_capacity_append_is_transactional() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                accesses: 1,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let resource = graph.buffer("buffer", BufferDesc { size: 8 }).unwrap();
        let access = |range| Access {
            resource: resource.into(),
            range: AccessRange::Buffer(range),
            kind: AccessKind::Write,
            purpose: AccessPurpose::CopyDestination,
        };
        assert_eq!(
            graph
                .declare_analysis_pass(
                    "too many",
                    PassKind::Copy,
                    &[access(0..4), access(4..8)],
                    false
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::CapacityExhausted
        );
        graph
            .upload_buffer("one", resource, 0, &[1, 2, 3, 4])
            .unwrap();
        let plan = graph.compile().unwrap();
        assert_eq!(plan.passes.len(), 1);
        assert_eq!(plan.accesses(0).len(), 1);
    }

    #[test]
    fn plan_remains_valid_after_pool_shutdown() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let resource = graph.buffer("data", BufferDesc { size: 4 }).unwrap();
        graph
            .upload_buffer("upload", resource, 0, &[1, 2, 3, 4])
            .unwrap();
        graph.export_buffer(resource).unwrap();
        let plan = graph.compile().unwrap();
        drop(pool);
        assert_eq!(plan.ordered_pass_names().collect::<Vec<_>>(), ["upload"]);
        assert_eq!(plan.upload_data(&plan.passes[0].operation), [1, 2, 3, 4]);
    }
}

//! Prepared backing shared by the unique builder and its immutable plans.

use super::{
    AttachmentOperand, BindingOperands, Dependency, Graph, GraphCapacityKind, GraphError, Pass,
    RasterDrawOperands, Resource, ResourceAccess, ResourceKey, analysis,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphCapacity {
    pub resources: usize,
    pub passes: usize,
    pub accesses: usize,
    pub draws: usize,
    pub bindings: usize,
    pub attachments: usize,
    pub dependencies: usize,
    pub initial_ranges: usize,
    pub name_bytes: usize,
    pub upload_bytes: usize,
    pub edges: usize,
    pub segments: usize,
    pub readers: usize,
}
impl Default for GraphCapacity {
    fn default() -> Self {
        Self {
            resources: 256,
            passes: 256,
            accesses: 2048,
            draws: 2048,
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

/// Opaque prepared backing. Its fields and all mutable access are private to
/// Renderer; dereferencing a Graph or plan only exposes an immutable value.
#[doc(hidden)]
pub struct GraphStorage {
    pub(crate) id: u64,
    pub(crate) resources: Vec<Resource>,
    pub(crate) passes: Vec<Pass>,
    pub(crate) exports: Vec<usize>,
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
    pub(super) bindings: Vec<BindingOperands>,
    pub(super) binding_accesses: Vec<ResourceAccess>,
    pub(super) raster_binding_seen: Vec<bool>,
    pub(super) raster_binding_sets: Vec<usize>,
    pub(super) attachments: Vec<AttachmentOperand>,
    pub(super) raster_ranges: Vec<[u64; 2]>,
    pub(super) raster_resources: Vec<usize>,
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
        Ok(Self {
            id: 0,
            resources: buffer(capacity.resources, GraphCapacityKind::Resources)?,
            passes: buffer(capacity.passes, GraphCapacityKind::Passes)?,
            exports: buffer(capacity.resources, GraphCapacityKind::Resources)?,
            exported: buffer(capacity.resources, GraphCapacityKind::Resources)?,
            imports,
            accesses: buffer(capacity.accesses, GraphCapacityKind::Accesses)?,
            dependencies: buffer(capacity.dependencies, GraphCapacityKind::Dependencies)?,
            initial_ranges: buffer(capacity.initial_ranges, GraphCapacityKind::InitialRanges)?,
            names: buffer(capacity.name_bytes, GraphCapacityKind::NameBytes)?,
            uploads: buffer(capacity.upload_bytes, GraphCapacityKind::UploadBytes)?,
            draws: buffer(capacity.draws, GraphCapacityKind::Draws)?,
            bindings: buffer(capacity.bindings, GraphCapacityKind::Bindings)?,
            binding_accesses: buffer(capacity.bindings, GraphCapacityKind::Bindings)?,
            raster_binding_seen,
            raster_binding_sets: buffer(capacity.bindings, GraphCapacityKind::Bindings)?,
            attachments: buffer(capacity.attachments, GraphCapacityKind::Attachments)?,
            raster_ranges,
            raster_resources: buffer(capacity.resources, GraphCapacityKind::Resources)?,
            overlap: buffer(capacity.accesses, GraphCapacityKind::Accesses)?,
            read_accesses: super::accesses::ReadAccessSet::new(capacity.accesses).map_err(
                |_| capacity_error(GraphCapacityKind::Accesses, capacity.accesses, None),
            )?,
            workspace: analysis::Workspace::new(capacity.analysis())
                .map_err(|error| capacity.analysis_error(error))?,
            output: analysis::Output::new(capacity.analysis())
                .map_err(|error| capacity.analysis_error(error))?,
            capacity,
            #[cfg(feature = "graph-diagnostics")]
            dump: std::sync::OnceLock::new(),
        })
    }
    fn reset(&mut self, id: u64) {
        self.clear_raster_ranges();
        self.id = id;
        self.passes.clear();
        self.resources.clear();
        self.exports.clear();
        self.exported.clear();
        self.imports.clear();
        self.accesses.clear();
        self.dependencies.clear();
        self.initial_ranges.clear();
        self.names.clear();
        self.uploads.clear();
        self.draws.clear();
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
    #[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
    pub(crate) fn raster_bindings(&self, range: &Range<usize>) -> &[BindingOperands] {
        &self.bindings[range.clone()]
    }
    #[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
    pub(crate) fn raster_attachments(&self, range: &Range<usize>) -> &[AttachmentOperand] {
        &self.attachments[range.clone()]
    }
    #[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
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
            capacity,
        })
    }
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
    pub fn begin(&mut self) -> Result<Graph, GraphError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("renderer.graph.begin"), 0));
        for slot in &mut self.slots {
            if let Some(unique) = Arc::get_mut(slot) {
                unique.data_mut().reset(super::next_id());
                return Ok(Graph {
                    slot: Arc::clone(slot),
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
                .declare_pass(
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

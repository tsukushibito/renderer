//! Native-independent, prepared Graph construction and immutable frame plans.
//!
//! Prepare a [`GraphPool`], borrow a [`Graph`] with [`GraphPool::begin`], then
//! declare resources and operations. Operands determine their resource accesses;
//! callers do not maintain a second read/write list for ordinary operations.
//! Reads consume contents established by preceding declarations, while
//! [`Graph::depends_on`] adds ordering without changing the selected producer.
//! Mark outputs with [`Graph::export_buffer`] or [`Graph::export_image`], or use
//! readback operations. [`Graph::compile`] checks contents and dependencies,
//! removes unobserved work, and freezes the same prepared backing.
//!
//! All handles are local to one checkout, even when a pool reuses its slot.
//! Compilation does not load a native backend or prove Device-specific limits;
//! native runtime preparation/submission establishes those guarantees separately.
//!
//! ```
//! use renderer::graph::{BufferDesc, GraphCapacity, GraphPool};
//! # fn main() -> Result<(), renderer::graph::GraphError> {
//! let mut pool = GraphPool::new(GraphCapacity::default(), 2)?;
//! let mut graph = pool.begin()?;
//! let output = graph.buffer("output", BufferDesc { size: 4 })?;
//! graph.upload_buffer("initialize", output, 0, &[1, 2, 3, 4])?;
//! graph.export_buffer(output)?;
//! let plan = graph.compile()?;
//! assert_eq!(plan.pass_count(), 1);
//! drop(plan); // Return this checkout once every other consumer has finished.
//! # Ok(())
//! # }
//! ```

use std::{
    ops::{Deref, Range},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

mod accesses;
mod analysis;
mod attachments;
mod builder;
mod compile;
mod dispatch;
mod error;
mod indirect;
mod output;
mod pass;
#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
pub(crate) mod provenance;
mod ranges;
mod raster;
mod resource;
mod storage;

pub use compile::CompiledGraph;
pub(crate) use dispatch::BindingOperands;
pub use dispatch::{Binding, BindingResource, RasterBindings};
pub use error::{GraphCapacityError, GraphCapacityKind, GraphError, GraphErrorKind};
pub use indirect::{IndexedIndirectArgs, IndexedIndirectTable, IndexedIndirectTemplate};
pub use output::{FrameGraph, FrameOutputOwnership};
pub use pass::{
    Access, AccessKind, AccessPurpose, AccessRange, Attachment, ImageAspect, LoadOp, PassKind,
    RasterAttachments, RasterConstants, RasterDraw, RasterParameters, RasterRect, ResourceHandle,
    StoreOp,
};
pub(crate) use pass::{
    AttachmentOperand, Dependency, DependencyList, IndirectDrawOperands, LoadAction, MeshUpload,
    Operation, Pass, RasterBatchOperands, RasterDrawOperands, RasterRegions, ResourceAccess,
    UploadSource,
};
pub use ranges::{ImportRanges, ImportRangesPool};
pub use raster::RasterPass;
pub use resource::{
    BufferDesc, BufferHandle, ImageDesc, ImageHandle, ImageView, ImportBuffer, ImportImage,
    PassHandle, ResourceKey, SampleCount,
};
pub(crate) use resource::{Resource, ResourceDesc};
pub use storage::{GraphCapacity, GraphPool, GraphStorage};
use storage::{Slot, capacity_error};

static NEXT_GRAPH: AtomicU64 = AtomicU64::new(1);
pub(crate) fn next_id() -> u64 {
    NEXT_GRAPH
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("graph identity exhausted")
}

/// Unique mutable checkout from a prepared [`GraphPool`]. Dropping an unfinished
/// Graph returns its slot; compiling consumes it and freezes the same backing.
///
/// Builders capture borrowed operands in prepared storage and roll back their
/// unpublished additions on error. Handles from successful earlier builders
/// remain usable. Required input, owner and capacity guarantees also apply when
/// development validation is disabled.
pub struct Graph {
    slot: Arc<Slot>,
}
impl Deref for Graph {
    type Target = GraphStorage;
    fn deref(&self) -> &Self::Target {
        self.slot.data()
    }
}
impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph")
            .field("id", &self.id)
            .field("resources", &self.resources.len())
            .field("passes", &self.passes.len())
            .finish()
    }
}
impl Graph {
    /// Identity retained by this Graph's compiled plan and submission.
    pub fn graph_id(&self) -> u64 {
        self.id
    }

    /// Correctness-test preparation only. Allocation acceptance must keep an
    /// explicit prepared pool alive across every measured frame and return.
    #[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
    pub(crate) fn test_new() -> Self {
        GraphPool::new(GraphCapacity::default(), 1)
            .unwrap()
            .begin()
            .unwrap()
    }
    fn data_mut(&mut self) -> &mut GraphStorage {
        // SAFETY: Graph is the unique non-Clone mutable checkout. A frozen plan
        // can only be created by consuming it, and the pool cannot reacquire a
        // slot while this Arc is held. The returned borrow is tied to &mut self.
        unsafe { &mut *self.slot.data.get() }
    }
    /// Requires `dependency` to execute before `pass`, keeping the dependency
    /// live whenever `pass` is live. Repeating the same edge has no effect.
    ///
    /// This constrains execution order only: reads still consume contents from
    /// preceding declarations, so a forward edge cannot select a later writer.
    /// Live cycles are rejected by [`Self::compile`]; additional development
    /// validation also diagnoses cycles in otherwise culled declarations.
    pub fn depends_on(
        &mut self,
        pass: PassHandle,
        dependency: PassHandle,
    ) -> Result<(), GraphError> {
        let pass = self.pass_id(pass)?;
        let dependency = self.pass_id(dependency)?;
        let list = self.passes[pass].dependencies;
        if !list
            .iter(&self.dependencies)
            .any(|target| target == dependency)
        {
            if self.dependencies.len() == self.capacity.dependencies {
                return Err(capacity_error(
                    GraphCapacityKind::Dependencies,
                    self.capacity.dependencies,
                    self.dependencies.len().checked_add(1),
                ));
            }
            let data = self.data_mut();
            let index = data.dependencies.len();
            data.dependencies.push(Dependency {
                target: dependency,
                next: None,
            });
            if let Some(tail) = list.tail {
                data.dependencies[tail].next = Some(index);
            } else {
                data.passes[pass].dependencies.head = Some(index);
            }
            data.passes[pass].dependencies.tail = Some(index);
        }
        Ok(())
    }
    /// Consume the declarations and produce an immutable execution plan.
    ///
    /// Compilation resolves construction-order contents, culls unobserved work,
    /// and orders the live passes with their required hazards. Uninitialized
    /// reads/exports and live cycles are rejected in every configuration;
    /// development validation additionally diagnoses otherwise culled cycles.
    ///
    /// Success freezes the existing backing, without cloning its arrays or
    /// upload payloads. Failure consumes the builder and returns its slot to
    /// the pool; correct the input or prepare the missing metadata capacity and
    /// rebuild. An error itself retains neither the Graph nor GPU resources.
    /// A plan's clones and native consumers pin its slot until they all return.
    /// This operation performs no native recording, submission or GPU wait.
    pub fn compile(self) -> Result<CompiledGraph, GraphError> {
        compile::compile(self)
    }
    fn push(
        &mut self,
        name: &str,
        operation: Operation,
        accesses: Range<usize>,
        side_effect: bool,
    ) -> Result<PassHandle, GraphError> {
        if self.passes.len() == self.capacity.passes {
            return Err(capacity_error(
                GraphCapacityKind::Passes,
                self.capacity.passes,
                self.passes.len().checked_add(1),
            ));
        }
        let name = self.store_name(name)?;
        let index = self.passes.len();
        let dependencies = DependencyList::default();
        self.data_mut().passes.push(Pass {
            name,
            operation,
            accesses,
            side_effect,
            dependencies,
        });
        Ok(PassHandle {
            graph: self.id,
            index,
        })
    }
    fn store_name(&mut self, name: &str) -> Result<Range<usize>, GraphError> {
        let start = self.names.len();
        let end = start.checked_add(name.len()).ok_or_else(|| {
            capacity_error(GraphCapacityKind::NameBytes, self.capacity.name_bytes, None)
        })?;
        if end > self.capacity.name_bytes {
            return Err(capacity_error(
                GraphCapacityKind::NameBytes,
                self.capacity.name_bytes,
                Some(end),
            ));
        }
        self.data_mut().names.extend_from_slice(name.as_bytes());
        Ok(start..end)
    }
    fn append_access(&mut self, access: ResourceAccess) -> Result<(), GraphError> {
        self.check_input_access(&access)?;
        if self.accesses.len() == self.capacity.accesses {
            return Err(capacity_error(
                GraphCapacityKind::Accesses,
                self.capacity.accesses,
                self.accesses.len().checked_add(1),
            ));
        }
        self.data_mut().accesses.push(access);
        Ok(())
    }
    fn append_accesses<const N: usize>(
        &mut self,
        accesses: [ResourceAccess; N],
    ) -> Result<Range<usize>, GraphError> {
        let start = self.accesses.len();
        for access in &accesses {
            self.check_input_access(access)?;
        }
        if N > self.capacity.accesses.saturating_sub(start) {
            return Err(capacity_error(
                GraphCapacityKind::Accesses,
                self.capacity.accesses,
                start.checked_add(N),
            ));
        }
        self.data_mut().accesses.extend(accesses);
        Ok(start..self.accesses.len())
    }
    fn check_input_access(&self, access: &ResourceAccess) -> Result<(), GraphError> {
        if access.write && self.resources[access.resource].read_only_input {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "snapshot mesh input is read-only; copy to an independent buffer",
                access.resource,
            ));
        }
        Ok(())
    }
    fn append_upload(&mut self, bytes: &[u8]) -> Result<UploadSource, GraphError> {
        let start = self.uploads.len();
        let end = start.checked_add(bytes.len()).ok_or_else(|| {
            capacity_error(
                GraphCapacityKind::UploadBytes,
                self.capacity.upload_bytes,
                None,
            )
        })?;
        if end > self.capacity.upload_bytes {
            return Err(capacity_error(
                GraphCapacityKind::UploadBytes,
                self.capacity.upload_bytes,
                Some(end),
            ));
        }
        self.data_mut().uploads.extend_from_slice(bytes);
        Ok(UploadSource::Frame(start..end))
    }
    fn pass_id(&self, handle: PassHandle) -> Result<usize, GraphError> {
        if handle.graph != self.id {
            return Err(
                GraphError::new(GraphErrorKind::WrongOwner, "foreign graph pass handle")
                    .with_pass(handle, None),
            );
        }
        if handle.index >= self.passes.len() {
            return Err(GraphError::new(
                GraphErrorKind::InvalidHandle,
                "undefined graph pass handle",
            )
            .with_pass(handle, None));
        }
        Ok(handle.index)
    }
}
pub(crate) fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}

#[cfg(test)]
mod tests;

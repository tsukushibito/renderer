//! GPU-independent declarations and analysis for the initial single-queue graph.
//!
//! Resources are mutable: reads consume the latest preceding write in declaration
//! order, including exact buffer ranges. Forward explicit dependencies constrain
//! scheduling, but never change which contents a read consumes. Every declaration,
//! including dead work, is validated. Only live operations are executed.
//! Imports describe runtime-owned resources and availability; they do not promise
//! CPU readiness. Image support is deliberately one complete mip/layer/aspect.

use std::{
    collections::BTreeMap,
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
};

mod accesses;
mod builder;
mod compile;
mod error;
mod pass;
mod resource;

pub use compile::CompiledGraph;
pub use error::{GraphError, GraphErrorKind};
pub use pass::{
    Access, AccessKind, AccessPurpose, AccessRange, Attachment, ComputeTransform, ImageAspect,
    LoadOp, PassKind, RasterAttachments, RasterDraw, RasterParameters, ResourceHandle, StoreOp,
};
pub(crate) use pass::{
    AttachmentOps, LoadAction, MeshUpload, Operation, Pass, RasterDrawOperands, ResourceAccess,
    ValidatedRasterParameters,
};
pub use resource::{
    BufferDesc, BufferHandle, ImageDesc, ImageFormat, ImageHandle, ImportBuffer, ImportImage,
    PassHandle, ResourceKey,
};
pub(crate) use resource::{Resource, ResourceDesc};

static NEXT_GRAPH: AtomicU64 = AtomicU64::new(1);

/// An append-only graph. Handles never become stale within their own graph;
/// each new graph receives a distinct identity. No pass callbacks run at build.
#[derive(Debug)]
pub struct Graph {
    pub(crate) id: u64,
    pub(crate) resources: Vec<Resource>,
    pub(crate) passes: Vec<Pass>,
    pub(crate) exports: Vec<usize>,
    exported: Vec<bool>,
    imports: BTreeMap<ResourceKey, usize>,
}
impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    pub fn new() -> Self {
        let id = NEXT_GRAPH
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("graph identity exhausted");
        Self {
            id,
            resources: Vec::new(),
            passes: Vec::new(),
            exports: Vec::new(),
            exported: Vec::new(),
            imports: BTreeMap::new(),
        }
    }
    /// `pass` must execute after `dependency`. Forward edges are allowed and
    /// Compile rejects live cycles in every configuration. With
    /// `development-validation`, it also rejects cycles confined to dead work.
    pub fn depends_on(
        &mut self,
        pass: PassHandle,
        dependency: PassHandle,
    ) -> Result<(), GraphError> {
        let pass = self.pass_id(pass)?;
        let dependency = self.pass_id(dependency)?;
        if !self.passes[pass].dependencies.contains(&dependency) {
            self.passes[pass].dependencies.push(dependency);
        }
        Ok(())
    }
    pub fn compile(&self) -> Result<CompiledGraph, GraphError> {
        compile::compile(self)
    }
    fn push(
        &mut self,
        name: impl Into<String>,
        operation: Operation,
        accesses: Vec<ResourceAccess>,
        side_effect: bool,
    ) -> PassHandle {
        let index = self.passes.len();
        self.passes.push(Pass {
            name: name.into(),
            operation,
            accesses,
            side_effect,
            dependencies: Vec::new(),
        });
        PassHandle {
            graph: self.id,
            index,
        }
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

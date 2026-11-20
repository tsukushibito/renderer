//! Shared bounded input setup for native Engine acceptance scenarios.

use std::ops::{Deref, DerefMut};

use crate::{
    engine::{Draw, Engine, EngineCapacity, EngineError, FrameSnapshot, SnapshotPool, View},
    graph::{
        Attachment, Graph, GraphCapacity, GraphError, GraphPool, ImageDesc, ImageHandle, LoadOp,
        RasterAttachments, StoreOp,
    },
    program::ManagedRasterProgram,
};

use super::{EngineRenderError, PreparedEngineFrame, RenderedFrame, VulkanEngine};

pub(crate) struct Assets {
    engine: Engine,
    snapshots: SnapshotPool,
}

impl Assets {
    pub(crate) fn new(meshes: usize, materials: usize) -> Result<Self, EngineError> {
        Ok(Self {
            engine: Engine::new(EngineCapacity {
                meshes,
                materials,
                retired_mesh_versions: 32,
            })?,
            snapshots: SnapshotPool::new(256, 32)?,
        })
    }

    pub(crate) fn snapshot(
        &self,
        view: &View,
        draws: &[Draw],
    ) -> Result<FrameSnapshot, EngineError> {
        self.engine.snapshot(&self.snapshots, view, draws)
    }
}

impl Deref for Assets {
    type Target = Engine;

    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}

impl DerefMut for Assets {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.engine
    }
}

pub(crate) fn graph() -> Graph {
    GraphPool::new(GraphCapacity::default(), 1)
        .unwrap()
        .begin()
        .unwrap()
}

/// Build a depth-tested mesh workload with the same frame scope as the tests'
/// residency and submission scenarios.
pub(crate) fn build_snapshot(
    engine: &mut VulkanEngine,
    snapshot: &FrameSnapshot,
    readback: bool,
    extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
) -> Result<PreparedEngineFrame, EngineRenderError> {
    let mut frame = engine.begin_graph()?;
    let inputs = engine.connect_snapshot(&mut frame, snapshot)?;
    let view = inputs.view();
    let graph = &mut frame.graph;
    let color = graph.image("forward color", ImageDesc::rgba8(view.width, view.height))?;
    let depth = graph.image("forward depth", ImageDesc::depth32(view.width, view.height))?;
    let attachments = RasterAttachments {
        colors: &[Attachment {
            view: graph.image_view(color, 0, 0)?,
            load: LoadOp::Clear(view.clear_color),
            store: StoreOp::Store,
        }],
        depth: Some(Attachment {
            view: graph.image_view(depth, 0, 0)?,
            load: LoadOp::Clear(1.0),
            store: StoreOp::Store,
        }),
        color_resolves: &[],
    };
    let draws = inputs.draws();
    let name = if draws.len() == 0 {
        "view clear"
    } else {
        "view opaque batch"
    };
    graph.raster(name, attachments, |raster| {
        raster.draw(&ManagedRasterProgram::mesh(), draws)
    })?;
    drop(inputs);
    let output = extension(graph, color)?;
    Ok(frame.select_output(output, readback)?)
}

pub(crate) fn render_snapshot(
    engine: &mut VulkanEngine,
    snapshot: &FrameSnapshot,
    readback: bool,
    extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
) -> Result<RenderedFrame, EngineRenderError> {
    let prepared = build_snapshot(engine, snapshot, readback, extension)?;
    engine.submit_frame(prepared.compile()?)
}

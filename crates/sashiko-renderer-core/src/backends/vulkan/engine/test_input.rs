//! Shared bounded input setup for native MeshRegistry acceptance scenarios.

use std::ops::{Deref, DerefMut};

use crate::{
    geometry::{MeshCapacity, MeshError, MeshHandle, MeshRegistry, MeshSnapshot, SnapshotPool},
    graph::{
        Attachment, Graph, GraphCapacity, GraphError, GraphPool, ImageDesc, ImageHandle, LoadOp,
        RasterAttachments, RasterParameters, StoreOp,
    },
    program::ManagedRasterProgram,
};

use super::{EngineRenderError, PreparedEngineFrame, RenderedFrame, VulkanEngine};

#[derive(Clone, Copy)]
pub(crate) struct View {
    pub width: u32,
    pub height: u32,
    pub clear_color: [f32; 4],
}

#[derive(Clone, Copy)]
pub(crate) struct Draw {
    pub mesh: MeshHandle,
    pub tint: [f32; 4],
    pub transform: [[f32; 4]; 4],
}

impl Draw {
    pub(crate) fn new(mesh: MeshHandle, tint: [f32; 4]) -> Self {
        Self {
            mesh,
            tint,
            transform: *RasterParameters::default().transform(),
        }
    }
}

pub(crate) struct FrameSnapshot {
    view: View,
    meshes: MeshSnapshot,
}

impl FrameSnapshot {
    pub(crate) fn capture(
        assets: &MeshRegistry,
        pool: &SnapshotPool,
        view: &View,
        draws: &[Draw],
    ) -> Result<Self, MeshError> {
        let meshes = assets.capture(
            pool,
            draws,
            |draw| draw.mesh,
            |draw, _| {
                Ok(RasterParameters::new(draw.transform, draw.tint)
                    .expect("valid test parameters")
                    .into())
            },
        )?;
        Ok(Self {
            view: *view,
            meshes,
        })
    }
}

impl Deref for FrameSnapshot {
    type Target = MeshSnapshot;

    fn deref(&self) -> &Self::Target {
        &self.meshes
    }
}

pub(crate) struct Assets {
    engine: MeshRegistry,
    snapshots: SnapshotPool,
}

impl Assets {
    pub(crate) fn new(meshes: usize) -> Result<Self, MeshError> {
        Ok(Self {
            engine: MeshRegistry::new(MeshCapacity {
                meshes,
                retired_mesh_versions: 32,
            })?,
            snapshots: SnapshotPool::new(256, 32)?,
        })
    }

    pub(crate) fn snapshot(&self, view: &View, draws: &[Draw]) -> Result<FrameSnapshot, MeshError> {
        FrameSnapshot::capture(&self.engine, &self.snapshots, view, draws)
    }
}

impl Deref for Assets {
    type Target = MeshRegistry;

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
    let view = &snapshot.view;
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

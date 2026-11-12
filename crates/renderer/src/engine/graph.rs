//! Lower owned engine input into ordinary, GPU-independent Graph operations.

use std::collections::BTreeMap;

use super::{FrameSnapshot, MeshHandle};
use crate::graph::{
    BufferDesc, BufferHandle, Graph, GraphError, GraphErrorKind, ImageDesc, ImageHandle, PassHandle,
};

/// A normal graph, its final linear RGBA8 output and optional readback pass.
/// The caller can inspect or extend it before compilation.
pub struct FrameGraph {
    pub graph: Graph,
    pub output: ImageHandle,
    pub readback: Option<PassHandle>,
}

impl FrameSnapshot {
    /// Builds a self-contained graph without a device or window. This standalone
    /// path uploads each distinct mesh once per graph. `VulkanEngine` reuses
    /// persistent GPU meshes instead. The extension runs during graph building,
    /// after opaque drawing and before final composite, not during GPU recording.
    ///
    /// It receives complete linear RGBA8 color at the View extent. Return a
    /// complete, initialized image of the same extent. Existing Graph checks
    /// validate handles, declared operation purposes and contents normally.
    pub fn build_graph(
        &self,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<FrameGraph, GraphError> {
        let mut graph = Graph::new();
        let mut meshes = BTreeMap::new();
        for draw in &self.draws {
            if meshes.contains_key(&draw.mesh) {
                continue;
            }
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
            meshes.insert(draw.mesh, (vertices, indices));
        }
        self.build_graph_with_meshes(graph, &meshes, readback, extension)
    }

    pub(crate) fn build_graph_with_meshes(
        &self,
        mut graph: Graph,
        meshes: &BTreeMap<MeshHandle, (BufferHandle, BufferHandle)>,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<FrameGraph, GraphError> {
        let desc = ImageDesc::rgba8(self.view.width, self.view.height);
        let color = graph.image("view color", desc)?;
        let depth = graph.image("view depth", ImageDesc::depth32(desc.width, desc.height))?;
        // This experiment keeps the opaque list in one rendering scope. The
        // builder retains a clear-only operation for an empty list.
        let mut draws = Vec::with_capacity(self.draws.len());
        for draw in &self.draws {
            let &(vertices, indices) = meshes.get(&draw.mesh).ok_or_else(|| {
                GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "missing resolved mesh buffers",
                )
            })?;
            draws.push((vertices, indices, draw.data.index_count, draw.parameters));
        }
        graph.raster_batch(
            if draws.is_empty() {
                "view clear"
            } else {
                "view opaque batch"
            },
            draws,
            color,
            depth,
            self.view.clear_color,
            1.0,
        )?;
        let transformed = extension(&mut graph, color)?;
        let output = graph.image("view output", desc)?;
        graph.composite("view final composite", transformed, output)?;
        graph.export_image(output)?;
        let readback = if readback {
            Some(graph.readback_image("view readback", output)?)
        } else {
            None
        };
        Ok(FrameGraph {
            graph,
            output,
            readback,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::{Engine, IDENTITY, View};

    #[test]
    fn empty_view_has_initialized_output_without_uploads() {
        let assets = Engine::new(1, 1).unwrap();
        let snapshot = assets
            .snapshot(
                &View {
                    width: 4,
                    height: 3,
                    clip_from_world: IDENTITY,
                    clear_color: [0.25, 0.5, 0.75, 1.0],
                },
                &[],
            )
            .unwrap();
        let frame = snapshot.build_graph(false, |_, color| Ok(color)).unwrap();
        let plan = frame.graph.compile().unwrap();
        let dump = plan.dump();
        assert!(dump.contains("view clear"));
        assert!(!dump.contains("upload"));
        assert!(frame.readback.is_none());
    }
}

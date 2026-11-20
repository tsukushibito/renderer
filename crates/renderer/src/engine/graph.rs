//! Lower owned engine input into ordinary, GPU-independent Graph operations.

use std::ops::Range;

use super::{FrameSnapshot, MeshData, SnapshotDraw, SnapshotMesh, View};
use crate::graph::{
    BufferDesc, BufferHandle, CompiledGraph, Graph, GraphError, ImageHandle, MeshUpload, PassHandle,
};

/// Responsibility for the final output's runtime registration. This does not
/// imply GPU completion or ownership of the native resource itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameOutputOwnership {
    /// This frame creates the registration; release it after scheduling consumers.
    Frame,
    /// The output is an imported registration retained by its original caller.
    Caller,
}

/// A finalized graph with one selected image output and optional readback.
///
/// The output, its registration ownership and the readback selection stay
/// together until [`Self::compile`] consumes the frame. Build with ordinary
/// Graph operations, then call [`Self::select_output`] to finalize the selection.
///
/// The final selection cannot be reassigned independently of its ownership:
/// ```compile_fail
/// use renderer::engine::FrameGraph;
/// use renderer::graph::ImageHandle;
/// fn change_output(frame: &mut FrameGraph, output: ImageHandle) {
///     frame.output = output;
/// }
/// ```
/// Replacing the graph after finalization is also disallowed:
/// ```compile_fail
/// use renderer::engine::FrameGraph;
/// use renderer::graph::Graph;
/// fn replace_graph(frame: &mut FrameGraph, graph: Graph) {
///     frame.graph = graph;
/// }
/// ```
pub struct FrameGraph {
    graph: Graph,
    output: ImageHandle,
    readback: Option<PassHandle>,
    output_ownership: FrameOutputOwnership,
}

impl FrameGraph {
    /// Export a graph-local image and optionally append a readback of it.
    /// Imported registrations remain caller-owned. Graph compilation validates
    /// the contents needed for the export and readback.
    pub fn select_output(
        mut graph: Graph,
        output: ImageHandle,
        readback: Option<&str>,
    ) -> Result<Self, GraphError> {
        graph.export_image(output)?;
        let output_ownership = if graph.resources[output.index].import.is_some() {
            FrameOutputOwnership::Caller
        } else {
            FrameOutputOwnership::Frame
        };
        let readback = readback
            .map(|name| graph.readback_image(name, output))
            .transpose()?;
        Ok(Self {
            graph,
            output,
            readback,
            output_ownership,
        })
    }

    /// Inspect the finalized declarations without changing their identity,
    /// selected output or readback association.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// The exported final output selected during graph construction.
    /// The handle belongs only to this frame's graph.
    pub fn output(&self) -> ImageHandle {
        self.output
    }

    /// The readback of [`Self::output`], if requested during construction.
    /// Save this handle before compilation to identify its eventual CPU result.
    ///
    /// Its association cannot be edited separately from the finalized output:
    /// ```compile_fail
    /// use renderer::engine::FrameGraph;
    /// fn remove_readback(frame: &mut FrameGraph) {
    ///     frame.readback = None;
    /// }
    /// ```
    pub fn readback(&self) -> Option<PassHandle> {
        self.readback
    }

    /// Reports who releases the final output registration after GPU consumers
    /// have been scheduled. Imported outputs remain owned by their caller.
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.output_ownership
    }

    /// Consume the finalized declarations and compile them without loading a
    /// native backend. Output and readback handles obtained above remain valid
    /// for the resulting plan. Failure returns the graph slot to its pool.
    pub fn compile(self) -> Result<CompiledGraph, GraphError> {
        self.graph.compile()
    }

    /// The native adapter transfers the complete finalized selection together.
    #[cfg(feature = "vulkan")]
    pub(crate) fn into_parts(
        self,
    ) -> (Graph, ImageHandle, Option<PassHandle>, FrameOutputOwnership) {
        (
            self.graph,
            self.output,
            self.readback,
            self.output_ownership,
        )
    }
}

impl MeshData {
    /// Upload one aligned slice of an immutable mesh stream. Only this bridge
    /// establishes mesh provenance; arbitrary public byte uploads cannot do so.
    pub(crate) fn upload_graph_slice(
        &self,
        graph: &mut Graph,
        name: impl AsRef<str>,
        dst: BufferHandle,
        indices: bool,
        source: Range<usize>,
    ) -> Result<PassHandle, GraphError> {
        let (bytes, provenance) = if indices {
            (
                &self.indices,
                MeshUpload::Indices {
                    byte_len: self.indices.len() as u64,
                    maximum: self.index_maximum,
                },
            )
        } else {
            (&self.vertices, MeshUpload::Vertices)
        };
        graph.upload_mesh_slice(name.as_ref(), dst, bytes, source, provenance)
    }
}

/// A temporary connection of one snapshot to a particular Graph. Draws borrow
/// this lease only during construction; Graph commands own their parameters.
/// Drop it before reconnecting the same snapshot. Other snapshots can be
/// connected independently, including different versions of the same mesh.
pub struct SnapshotInputs<'a> {
    snapshot: &'a FrameSnapshot,
    meshes: Vec<SnapshotMesh>,
}

impl SnapshotInputs<'_> {
    pub fn view(&self) -> &View {
        &self.snapshot.view
    }

    /// Immutable indexed operands for the ordinary public Raster builder.
    /// Their mesh buffers are read-only Graph inputs. Copy to another buffer
    /// when mutable contents or a persistent export are needed.
    pub fn draws(&self) -> impl ExactSizeIterator<Item = crate::graph::RasterDraw> + '_ {
        self.snapshot.draws.iter().map(|draw| {
            let index = self
                .meshes
                .binary_search_by_key(&draw.mesh, |entry| entry.mesh)
                .expect("connected snapshot mesh");
            let (vertices, indices) = self.meshes[index].buffers.expect("connected mesh buffers");
            crate::graph::RasterDraw {
                vertices,
                indices,
                first_index: 0,
                index_count: draw.data.index_count,
                instance_count: 1,
                parameters: draw.parameters.into(),
                bindings: None,
            }
        })
    }
}

impl Drop for SnapshotInputs<'_> {
    fn drop(&mut self) {
        let mut meshes = std::mem::take(&mut self.meshes);
        meshes.clear();
        *self
            .snapshot
            .meshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(meshes);
    }
}

impl FrameSnapshot {
    /// Connect immutable mesh and draw inputs to a caller-created Graph.
    /// Adds mesh buffers/uploads only; no Raster, image, export or readback is
    /// selected. Unused uploads are culled normally. Native residency uses
    /// `VulkanEngine::connect_snapshot` instead of uploading every frame.
    ///
    /// One live connection lease is supported per snapshot; a second connection
    /// returns capacity backpressure without waiting. The scratch is returned
    /// on every success, failure or unwind path. Commands already constructed
    /// from these inputs remain valid after the lease and snapshot are dropped.
    pub fn connect_graph<'a>(
        &'a self,
        graph: &mut Graph,
    ) -> Result<SnapshotInputs<'a>, GraphError> {
        self.connect_with(graph, |graph, draw| {
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
            draw.data.upload_graph_slice(
                graph,
                "mesh vertex upload",
                vertices,
                false,
                0..draw.data.vertices.len(),
            )?;
            draw.data.upload_graph_slice(
                graph,
                "mesh index upload",
                indices,
                true,
                0..draw.data.indices.len(),
            )?;
            graph.protect_input(vertices, true)?;
            graph.protect_input(indices, true)?;
            Ok((vertices, indices))
        })
    }

    pub(crate) fn connect_with<'a, E: From<GraphError>>(
        &'a self,
        graph: &mut Graph,
        mut resolve: impl FnMut(&mut Graph, &SnapshotDraw) -> Result<(BufferHandle, BufferHandle), E>,
    ) -> Result<SnapshotInputs<'a>, E> {
        let meshes = self
            .meshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GraphError::capacity_error(
                    crate::graph::GraphCapacityKind::SnapshotInputs,
                    1,
                    Some(2),
                )
            })?;
        let mut inputs = SnapshotInputs {
            snapshot: self,
            meshes,
        };
        // A forgotten public lease may permanently lose this scratch backing.
        // Return backpressure instead of growing a replacement on the frame path.
        if inputs.meshes.capacity() < self.draws.len() {
            return Err(GraphError::capacity_error(
                crate::graph::GraphCapacityKind::SnapshotInputs,
                0,
                Some(1),
            )
            .into());
        }
        inputs.meshes.extend(
            self.draws
                .iter()
                .enumerate()
                .map(|(index, draw)| SnapshotMesh {
                    mesh: draw.mesh,
                    draw: index,
                    buffers: None,
                }),
        );
        inputs.meshes.sort_unstable_by_key(|entry| entry.mesh);
        inputs.meshes.dedup_by_key(|entry| entry.mesh);
        for entry in &mut inputs.meshes {
            entry.buffers = Some(resolve(graph, &self.draws[entry.draw])?);
        }
        Ok(inputs)
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::{FrameGraph, FrameOutputOwnership, MeshData, Vertex};
    use crate::graph::{
        GraphCapacity, GraphErrorKind, GraphPool, ImageDesc, ImportImage, ResourceKey,
    };

    fn graph_pool() -> GraphPool {
        GraphPool::new(
            GraphCapacity {
                resources: 512,
                passes: 512,
                ..GraphCapacity::default()
            },
            2,
        )
        .unwrap()
    }

    fn triangle() -> MeshData {
        MeshData::new(
            &[[-0.5, -0.5, 0.25], [0.5, -0.5, 0.25], [0.0, 0.5, 0.25]].map(|position| Vertex {
                position,
                color: [1.0; 4],
            }),
            &[0, 1, 2],
        )
        .unwrap()
    }

    #[test]
    fn immutable_mesh_upload_preserves_stream_provenance_and_failed_append_atomicity() {
        use crate::graph::{MeshUpload, Operation, UploadSource};
        use std::sync::Arc;

        let data = triangle();
        let mut graph_pool = graph_pool();
        let mut graph = graph_pool.begin().unwrap();
        let indices = graph
            .buffer(
                "indices",
                crate::graph::BufferDesc {
                    size: data.indices.len() as u64,
                },
            )
            .unwrap();
        let uploaded = data
            .upload_graph_slice(&mut graph, "index chunk", indices, true, 4..8)
            .unwrap();
        let Operation::UploadBuffer {
            data: UploadSource::Shared { bytes, source },
            offset,
            mesh,
            ..
        } = &graph.passes[uploaded.index].operation
        else {
            panic!("mesh chunk upload")
        };
        assert!(Arc::ptr_eq(bytes, &data.indices));
        assert_eq!(source, &(4..8));
        assert_eq!(*offset, 4);
        assert!(matches!(
            mesh,
            Some(MeshUpload::Indices {
                byte_len: 12,
                maximum: 2
            })
        ));
        assert_eq!(graph.passes.len(), 1);

        for invalid in [1..5, 12..16] {
            let error = data
                .upload_graph_slice(&mut graph, "invalid chunk", indices, true, invalid)
                .unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
            assert_eq!(error.resource(), Some(indices.into()));
            assert_eq!(graph.passes.len(), 1, "failed mesh upload appends nothing");
        }
        graph
            .upload_buffer("raw bytes", indices, 0, &[0; 4])
            .unwrap();
        assert!(matches!(
            graph.passes.last().unwrap().operation,
            Operation::UploadBuffer { mesh: None, .. }
        ));
    }

    #[test]
    fn selecting_imported_output_keeps_caller_registration_ownership() {
        let mut graph_pool = graph_pool();
        let imported = ImportImage::test_initialized(
            ResourceKey {
                owner: 9,
                index: 3,
                generation: 2,
            },
            4,
            ImageDesc::rgba8(16, 16),
        );
        for readback in [false, true] {
            let mut graph = graph_pool.begin().unwrap();
            let imported_output = graph.import_image("caller image", &imported).unwrap();
            let frame = FrameGraph::select_output(
                graph,
                imported_output,
                readback.then_some("output readback"),
            )
            .unwrap();
            let output = frame.output();
            assert_eq!(output, imported_output);
            assert_eq!(frame.output_ownership(), FrameOutputOwnership::Caller);
            assert_eq!(frame.graph().image_desc(output).unwrap(), imported.desc());
            let readback_pass = frame.readback();
            assert_eq!(readback_pass.is_some(), readback);
            let plan = frame.compile().unwrap();
            // No copy or rendering is required to export an already valid image.
            assert_eq!(plan.pass_count(), usize::from(readback));
            assert!(plan.has_export(output.index));
            if let Some(pass) = readback_pass {
                assert!(matches!(
                    plan.passes[pass.index].operation,
                    crate::graph::Operation::ReadbackImage { src } if src == output.index
                ));
            }
        }
    }
}

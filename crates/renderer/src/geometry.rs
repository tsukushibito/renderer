//! Immutable, shader-independent indexed triangle streams.
//!
//! Establish index bounds at the asset creation/update boundary. Uploads preserve
//! this private summary, so native draw preparation needs neither an index scan
//! nor a retained CPU copy solely to prove those bounds. Scene, material and
//! vertex-attribute interpretation remain the caller's responsibility.

mod registry;
pub use registry::{
    MeshCapacity, MeshError, MeshErrorKind, MeshHandle, MeshRegistry, MeshSnapshot, SnapshotInputs,
    SnapshotPool,
};

use std::{fmt, ops::Range, sync::Arc};

use crate::graph::{BufferHandle, Graph, GraphError, GraphErrorKind, MeshUpload, PassHandle};

/// Immutable stream selected for a partial geometry upload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryStream {
    Vertices,
    Indices,
}

/// Owned encoded vertices and little-endian `u32` triangle indices.
///
/// The immutable index summary covers exactly the full index stream. Use
/// [`Self::index_count`] for a draw of that stream. The runtime still checks the
/// actual bound vertex buffer, selected Program stride and Device index limits.
/// Writes/copies to a GPU buffer invalidate or replace its previous contents and
/// index proof; this value does not authorize later arbitrary mutations.
///
/// The CPU streams are upload inputs, not runtime validation payloads. Graph
/// uploads retain them until the captured plan/storage is released. After GPU
/// completion, release upload plans, their idle GraphPool and this value at a
/// preparation/maintenance boundary before starting an allocation-free frame
/// window. Reusing an uncleared upload slot later can otherwise release its last
/// CPU owner during that later frame.
#[derive(Clone)]
pub struct GeometryData {
    pub(crate) vertices: Arc<[u8]>,
    pub(crate) indices: Arc<[u8]>,
    vertex_stride: u32,
    vertex_count: u64,
    index_count: u32,
    index_maximum: u32,
}

impl fmt::Debug for GeometryData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeometryData")
            .field("vertex_stride", &self.vertex_stride)
            .field("vertex_count", &self.vertex_count)
            .field("index_count", &self.index_count)
            .finish()
    }
}

impl GeometryData {
    /// Copy immutable vertex bytes and encode triangle indices once.
    ///
    /// `vertex_stride` must be nonzero and four-byte aligned; `vertices` must
    /// contain a nonempty, exact number of those vertices. Indices must be a
    /// nonempty triangle list fitting `u32` counts, and each index must reference
    /// one of those vertices. Index bytes are encoded little-endian.
    ///
    /// Vertex fields and padding are opaque here. The caller establishes its
    /// vertex layout and asset semantics before upload, and chooses a matching
    /// trusted Program. Device-dependent Program/stride limits are checked by
    /// the runtime at preparation, not inferred from an offline asset.
    pub fn new(vertex_stride: u32, vertices: &[u8], indices: &[u32]) -> Result<Self, GraphError> {
        if vertex_stride == 0 || !vertex_stride.is_multiple_of(4) {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "geometry vertex stride must be nonzero and four-byte aligned",
            ));
        }
        let stride = usize::try_from(vertex_stride).map_err(|_| {
            GraphError::new(
                GraphErrorKind::InvalidInput,
                "geometry vertex stride exceeds the host address space",
            )
        })?;
        if vertices.is_empty() || !vertices.len().is_multiple_of(stride) {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "geometry vertices must contain complete nonempty vertex records",
            ));
        }
        if indices.is_empty() || !indices.len().is_multiple_of(3) {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "geometry requires nonempty triangle indices",
            ));
        }
        let index_count = u32::try_from(indices.len()).map_err(|_| {
            GraphError::new(
                GraphErrorKind::InvalidInput,
                "geometry index count exceeds u32",
            )
        })?;
        let vertex_count = (vertices.len() / stride) as u64;
        let index_maximum = indices.iter().copied().max().expect("nonempty indices");
        if u64::from(index_maximum) >= vertex_count {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "geometry index exceeds its vertex count",
            ));
        }
        let index_bytes = indices.len().checked_mul(4).ok_or_else(|| {
            GraphError::new(
                GraphErrorKind::InvalidInput,
                "geometry index byte size overflow",
            )
        })?;
        let mut encoded_indices = Vec::with_capacity(index_bytes);
        for index in indices {
            encoded_indices.extend_from_slice(&index.to_le_bytes());
        }
        Ok(Self {
            vertices: Arc::from(vertices),
            indices: encoded_indices.into(),
            vertex_stride,
            vertex_count,
            index_count,
            index_maximum,
        })
    }

    pub fn vertex_stride(&self) -> u32 {
        self.vertex_stride
    }

    pub fn vertex_count(&self) -> u64 {
        self.vertex_count
    }

    pub fn index_count(&self) -> u32 {
        self.index_count
    }

    pub fn vertex_byte_len(&self) -> u64 {
        self.vertices.len() as u64
    }

    pub fn index_byte_len(&self) -> u64 {
        self.indices.len() as u64
    }

    /// Encoded payload bytes, independent of sharing and native allocation size.
    pub fn byte_len(&self) -> u64 {
        self.vertex_byte_len() + self.index_byte_len()
    }

    /// Capture the complete immutable vertex stream into an equally sized buffer.
    ///
    /// This retains the owned source without copying it into the Graph's upload
    /// byte arena. It does not establish arbitrary vertex bytes as index data.
    /// Wrong owners, buffer lengths or prepared capacity fail without adding a
    /// pass. The buffer remains subject to ordinary Graph lifetime and hazards.
    pub fn upload_vertices(
        &self,
        graph: &mut Graph,
        name: &str,
        destination: BufferHandle,
    ) -> Result<PassHandle, GraphError> {
        self.upload_graph_slice(graph, name, destination, false, 0..self.vertices.len())
    }

    /// Capture all indices and their private content maximum into an equally
    /// sized buffer, without retaining a CPU validation payload after upload.
    ///
    /// This uses the same transactional checks and source ownership as
    /// [`Self::upload_vertices`]. The native proof covers the full stream only;
    /// drawing a different prefix requires separately established index contents.
    pub fn upload_indices(
        &self,
        graph: &mut Graph,
        name: &str,
        destination: BufferHandle,
    ) -> Result<PassHandle, GraphError> {
        self.upload_graph_slice(graph, name, destination, true, 0..self.indices.len())
    }

    /// Upload an aligned byte range of a stream at the same offset in its
    /// full-sized destination. Consecutive index chunks preserve the validated
    /// index bound once the entire stream has been accepted.
    pub fn upload_range(
        &self,
        graph: &mut Graph,
        name: &str,
        dst: BufferHandle,
        stream: GeometryStream,
        range: Range<usize>,
    ) -> Result<PassHandle, GraphError> {
        self.upload_graph_slice(graph, name, dst, stream == GeometryStream::Indices, range)
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{BufferDesc, GraphCapacity, GraphCapacityKind, GraphPool, Operation};

    #[test]
    fn encoded_geometry_checks_layout_and_actual_index_bounds() {
        for (stride, vertices, indices) in [
            (0, &[0; 12][..], &[0, 1, 2][..]),
            (3, &[0; 12], &[0, 1, 2]),
            (4, &[], &[0, 1, 2]),
            (4, &[0; 13], &[0, 1, 2]),
            (4, &[0; 12], &[]),
            (4, &[0; 12], &[0, 1]),
            (4, &[0; 12], &[0, 1, 3]),
        ] {
            assert_eq!(
                GeometryData::new(stride, vertices, indices)
                    .unwrap_err()
                    .kind(),
                GraphErrorKind::InvalidInput
            );
        }
        let geometry = GeometryData::new(48, &[0; 144], &[2, 1, 0]).unwrap();
        assert_eq!(geometry.vertex_stride(), 48);
        assert_eq!(geometry.vertex_count(), 3);
        assert_eq!(geometry.index_count(), 3);
        assert_eq!(geometry.vertex_byte_len(), 144);
        assert_eq!(geometry.index_byte_len(), 12);
        assert_eq!(geometry.byte_len(), 156);
    }

    #[test]
    fn immutable_uploads_survive_source_mutation_and_owner_return_without_blob_copies() {
        let mut vertices = vec![7; 144];
        let mut indices = vec![2, 1, 0];
        let geometry = GeometryData::new(48, &vertices, &indices).unwrap();
        vertices.fill(99);
        indices.fill(u32::MAX);
        let mut pool = GraphPool::new(
            GraphCapacity {
                upload_bytes: 0,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let vb = graph.buffer("vertices", BufferDesc { size: 144 }).unwrap();
        let ib = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
        let vertex_upload = geometry
            .upload_vertices(&mut graph, "vertices", vb)
            .unwrap();
        let index_upload = geometry.upload_indices(&mut graph, "indices", ib).unwrap();
        graph
            .readback_buffer("readback vertices", vb, 0, 144)
            .unwrap();
        graph
            .readback_buffer("readback indices", ib, 0, 12)
            .unwrap();
        drop(geometry);
        let compiled = graph.compile().unwrap();
        assert_eq!(
            compiled.upload_data(&compiled.passes[vertex_upload.index].operation),
            &[7; 144]
        );
        assert_eq!(
            compiled.upload_data(&compiled.passes[index_upload.index].operation),
            &[2, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]
        );
        assert!(matches!(
            compiled.passes[index_upload.index].operation,
            Operation::UploadBuffer {
                mesh: Some(MeshUpload::Indices {
                    byte_len: 12,
                    maximum: 2
                }),
                ..
            }
        ));
        drop(compiled);
        drop(pool);
    }

    #[test]
    fn upload_rejects_wrong_owner_size_and_capacity_without_adding_passes() {
        let geometry = GeometryData::new(48, &[0; 144], &[2, 1, 0]).unwrap();
        let mut pool = GraphPool::new(
            GraphCapacity {
                passes: 1,
                upload_bytes: 0,
                ..GraphCapacity::default()
            },
            2,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let mut foreign = pool.begin().unwrap();
        let foreign_buffer = foreign.buffer("foreign", BufferDesc { size: 12 }).unwrap();
        assert_eq!(
            geometry
                .upload_indices(&mut graph, "foreign", foreign_buffer)
                .unwrap_err()
                .kind(),
            GraphErrorKind::WrongOwner
        );
        let short = graph.buffer("short", BufferDesc { size: 8 }).unwrap();
        assert_eq!(
            geometry
                .upload_indices(&mut graph, "short", short)
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
        assert!(graph.passes.is_empty());
        let ib = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
        let vb = graph.buffer("vertices", BufferDesc { size: 144 }).unwrap();
        geometry.upload_indices(&mut graph, "indices", ib).unwrap();
        let error = geometry
            .upload_vertices(&mut graph, "full", vb)
            .unwrap_err();
        assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Passes);
        assert_eq!(graph.passes.len(), 1);
    }
}

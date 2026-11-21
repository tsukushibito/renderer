use renderer_core::geometry::GeometryData;
use renderer_core::graph::{BufferDesc, GraphCapacity, GraphErrorKind, GraphPool};

#[test]
fn custom_vertex_stream_uploads_with_no_graph_blob_capacity_or_native_backend() {
    let geometry = GeometryData::new(48, &[0; 144], &[0, 2, 1]).unwrap();
    let mut pool = GraphPool::new(
        GraphCapacity {
            upload_bytes: 0,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let vertices = graph
        .buffer(
            "vertices",
            BufferDesc {
                size: geometry.vertex_byte_len(),
            },
        )
        .unwrap();
    let indices = graph
        .buffer(
            "indices",
            BufferDesc {
                size: geometry.index_byte_len(),
            },
        )
        .unwrap();
    let wrong_size = graph.buffer("wrong size", BufferDesc { size: 16 }).unwrap();
    assert_eq!(
        geometry
            .upload_indices(&mut graph, "invalid", wrong_size)
            .unwrap_err()
            .kind(),
        GraphErrorKind::InvalidInput
    );
    geometry
        .upload_vertices(&mut graph, "vertices", vertices)
        .unwrap();
    geometry
        .upload_indices(&mut graph, "indices", indices)
        .unwrap();
    graph
        .readback_buffer("vertex consumer", vertices, 0, geometry.vertex_byte_len())
        .unwrap();
    graph
        .readback_buffer("index consumer", indices, 0, geometry.index_byte_len())
        .unwrap();
    drop(geometry);
    let compiled = graph.compile().unwrap();
    assert_eq!(compiled.pass_count(), 4);
}

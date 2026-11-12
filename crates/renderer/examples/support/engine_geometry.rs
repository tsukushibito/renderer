//! Deterministic geometry and independent full-image expectations for workloads.

use crate::engine::{MeshData, Vertex};

pub(super) struct GridData {
    pub mesh: MeshData,
    pub vertices: Vec<u8>,
    pub indices: Vec<u8>,
    pub vertex_count: usize,
    pub index_count: usize,
}

/// A connected, tessellated square rather than repeated draws of one triangle.
/// Each vertex has white color, so immutable material snapshots select the tint.
/// Four hundred cells produce 160801 vertices, 960000 indices, and a vertex
/// buffer larger than the runtime's default four-MiB staging buffer.
pub(super) fn grid(cells: u32) -> GridData {
    assert!(
        (1..=1_024).contains(&cells),
        "fixture cell count out of range"
    );
    let side = cells + 1;
    let vertex_count = side as usize * side as usize;
    let index_count = cells as usize * cells as usize * 6;
    let mut vertices = Vec::with_capacity(vertex_count);
    let mut encoded_vertices = Vec::with_capacity(vertex_count * 28);
    for y in 0..=cells {
        for x in 0..=cells {
            let vertex = Vertex {
                position: [
                    -0.75 + 1.5 * x as f32 / cells as f32,
                    -0.75 + 1.5 * y as f32 / cells as f32,
                    0.25,
                ],
                color: [1.0; 4],
            };
            for value in vertex.position.iter().chain(&vertex.color) {
                encoded_vertices.extend_from_slice(&value.to_ne_bytes());
            }
            vertices.push(vertex);
        }
    }
    let mut indices = Vec::with_capacity(index_count);
    for y in 0..cells {
        for x in 0..cells {
            let a = y * side + x;
            let b = a + 1;
            let c = a + side;
            let d = c + 1;
            indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    let encoded_indices = indices.iter().flat_map(|i| i.to_ne_bytes()).collect();
    GridData {
        mesh: MeshData::new(&vertices, &indices).expect("valid generated grid"),
        vertices: encoded_vertices,
        indices: encoded_indices,
        vertex_count,
        index_count,
    }
}

/// Independent expectation for a square whose transformed edges are exactly on
/// pixel boundaries. Bounds are [left, top, right, bottom], with exclusive ends.
/// The caller accounts for any Compute color transform in both supplied colors.
pub(super) fn rectangle(
    width: u32,
    height: u32,
    bounds: [u32; 4],
    inside: [u8; 4],
    outside: [u8; 4],
) -> Vec<u8> {
    let [left, top, right, bottom] = bounds;
    assert!(left <= right && right <= width && top <= bottom && bottom <= height);
    let mut bytes = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            bytes.extend_from_slice(
                if (left..right).contains(&x) && (top..bottom).contains(&y) {
                    &inside
                } else {
                    &outside
                },
            );
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_connects_unique_vertices_across_cell_boundaries() {
        let data = grid(2);
        assert_eq!(data.vertex_count, 9);
        assert_eq!(data.index_count, 24);
        let indices = data
            .indices
            .chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(&indices[..12], &[0, 1, 3, 1, 4, 3, 1, 2, 4, 2, 5, 4]);
        assert_eq!(indices.iter().copied().max(), Some(8));
        let position = |index: usize| {
            std::array::from_fn::<_, 3, _>(|component| {
                let start = index * 28 + component * 4;
                f32::from_ne_bytes(data.vertices[start..start + 4].try_into().unwrap())
            })
        };
        assert_eq!(position(0), [-0.75, -0.75, 0.25]);
        assert_eq!(position(4), [0.0, 0.0, 0.25]);
        assert_eq!(position(8), [0.75, 0.75, 0.25]);
    }

    #[test]
    fn large_grid_exceeds_default_staging_with_real_geometry() {
        let data = grid(400);
        assert_eq!(data.vertex_count, 160_801);
        assert_eq!(data.index_count, 960_000);
        assert_eq!(data.vertices.len(), 4_502_428);
        assert_eq!(data.indices.len(), 3_840_000);
        assert!(data.vertices.len() > 4 * 1024 * 1024);
        // Retain the validated Renderer input alongside independent expectations.
        let _ = &data.mesh;
    }

    #[test]
    fn expected_rectangle_checks_all_pixels_and_half_open_edges() {
        let bytes = rectangle(4, 3, [1, 1, 3, 2], [255, 0, 0, 255], [0, 0, 0, 255]);
        assert_eq!(bytes.len(), 48);
        let red_pixels = bytes
            .chunks_exact(4)
            .enumerate()
            .filter_map(|(index, color)| (color == [255, 0, 0, 255]).then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(red_pixels, [5, 6]);
        assert_eq!(&bytes[7 * 4..8 * 4], &[0, 0, 0, 255]);
    }
}

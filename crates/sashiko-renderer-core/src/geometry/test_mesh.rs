//! Position and color records for the built-in mesh program in core tests.
use super::GeometryData;
use crate::graph::GraphError;

#[derive(Clone, Copy)]
pub(crate) struct Vertex {
    pub position: [f32; 3],
    pub color: [f32; 4],
}

pub(crate) fn mesh_data(vertices: &[Vertex], indices: &[u32]) -> Result<GeometryData, GraphError> {
    let bytes = vertices
        .iter()
        .flat_map(|v| v.position.into_iter().chain(v.color))
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>();
    GeometryData::new(28, &bytes, indices)
}

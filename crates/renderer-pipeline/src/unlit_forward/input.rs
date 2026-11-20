//! Immutable inputs for the vertex-color Forward recipe.
use renderer::geometry::{
    GeometryData, MeshError, MeshHandle, MeshRegistry, MeshSnapshot, SnapshotPool,
};
use renderer::graph::RasterParameters;

/// Four columns, each containing four rows. Vectors are column vectors.
/// `clip_from_world * world_from_model` transforms a local vertex to clip space.
pub type Matrix4 = [[f32; 4]; 4];

const VERTEX_STRIDE: u32 = 28;

pub const IDENTITY: Matrix4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    /// Linear RGBA; no implicit sRGB decoding is performed.
    pub color: [f32; 4],
}

/// Encode finite vertex colors and positions for the built-in mesh program.
pub fn mesh_data(vertices: &[Vertex], indices: &[u32]) -> Result<GeometryData, MeshError> {
    let mut bytes = Vec::with_capacity(
        vertices
            .len()
            .checked_mul(VERTEX_STRIDE as usize)
            .ok_or_else(|| MeshError::invalid_input("vertex byte size overflow"))?,
    );
    for vertex in vertices {
        for value in vertex.position.iter().chain(&vertex.color) {
            if !value.is_finite() {
                return Err(MeshError::invalid_input(
                    "mesh vertex components must be finite",
                ));
            }
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    GeometryData::new(VERTEX_STRIDE, &bytes, indices)
        .map_err(|error| MeshError::invalid_input(error.to_string()))
}

/// A complete offscreen view. Clip depth is 0..W; viewport height is positive.
/// Projection/handedness are chosen by the caller through `clip_from_world`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub width: u32,
    pub height: u32,
    pub clip_from_world: Matrix4,
    pub clear_color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct Draw {
    pub mesh: MeshHandle,
    pub tint: [f32; 4],
    pub world_from_model: Matrix4,
}

/// A captured Unlit view and its immutable geometry versions and draw constants.
#[derive(Debug)]
pub struct FrameSnapshot {
    view: View,
    meshes: MeshSnapshot,
}

impl FrameSnapshot {
    pub fn capture(
        assets: &MeshRegistry,
        pool: &SnapshotPool,
        view: &View,
        draws: &[Draw],
    ) -> Result<Self, MeshError> {
        if view.width == 0
            || view.height == 0
            || u64::from(view.width)
                .checked_mul(u64::from(view.height))
                .and_then(|n| n.checked_mul(4))
                .is_none()
            || !finite_matrix(view.clip_from_world)
            || view.clear_color.iter().any(|x| !x.is_finite())
        {
            return Err(MeshError::invalid_input(
                "view needs a valid extent and finite parameters",
            ));
        }

        let meshes = assets.capture(
            pool,
            draws,
            |draw| draw.mesh,
            |draw, geometry| {
                if geometry.vertex_stride() != VERTEX_STRIDE {
                    return Err(MeshError::invalid_input(
                        "Unlit requires position and linear RGBA vertex records",
                    ));
                }
                if !finite_matrix(draw.world_from_model) {
                    return Err(MeshError::invalid_input("model matrix must be finite"));
                }
                let parameters = RasterParameters::new(
                    multiply(view.clip_from_world, draw.world_from_model),
                    draw.tint,
                )
                .map_err(|_| MeshError::invalid_input("transform and tint must be finite"))?;
                Ok(parameters.into())
            },
        )?;
        Ok(Self {
            view: *view,
            meshes,
        })
    }

    pub fn view(&self) -> &View {
        &self.view
    }
    /// Geometry input shared with the resident and graph-local execution paths.
    pub fn meshes(&self) -> &MeshSnapshot {
        &self.meshes
    }
}

fn finite_matrix(matrix: Matrix4) -> bool {
    matrix.iter().flatten().all(|x| x.is_finite())
}
fn multiply(a: Matrix4, b: Matrix4) -> Matrix4 {
    let mut output = [[0.0; 4]; 4];
    for (column, result) in output.iter_mut().enumerate() {
        for (row, value) in result.iter_mut().enumerate() {
            *value = (0..4).map(|k| a[k][row] * b[column][k]).sum();
        }
    }
    output
}

impl std::ops::Deref for FrameSnapshot {
    type Target = MeshSnapshot;
    fn deref(&self) -> &MeshSnapshot {
        self.meshes()
    }
}

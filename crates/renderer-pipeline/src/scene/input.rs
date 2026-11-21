use super::{DefaultViews, MaterialHandle, PointLight, PreparedView, Transform};
use renderer::graph::{BufferHandle, GraphError};

/// A graph-local indexed mesh range. Vertex records must match the material's program.
#[derive(Clone, Copy, Debug)]
pub struct Mesh {
    pub vertex_stride: u32,
    pub vertices: BufferHandle,
    pub indices: BufferHandle,
    pub first_index: u32,
    pub index_count: u32,
}
#[derive(Clone, Copy, Debug)]
pub struct Draw {
    pub mesh: Mesh,
    pub material: MaterialHandle,
    /// None selects world-space vertex records.
    pub transform: Option<Transform>,
    /// Local-space point used for transparent sorting. Defaults to the mesh origin.
    pub sort_center: [f32; 3],
    /// Multiplier for the built-in vertex-color material.
    pub tint: [f32; 4],
    pub casts_shadow: bool,
}
impl Draw {
    pub(super) fn view_depth(&self, view: &PreparedView) -> Result<f64, GraphError> {
        let m = self.transform.unwrap_or(Transform::IDENTITY).columns();
        let [x, y, z] = self.sort_center;
        let point = [x, y, z, 1.];
        let depth = (0..3)
            .map(|r| {
                view.sort_direction[r]
                    * (0..4)
                        .map(|c| f64::from(m[c][r]) * f64::from(point[c]))
                        .sum::<f64>()
            })
            .sum::<f64>();
        if !depth.is_finite() {
            return Err(GraphError::invalid_input(
                "nonfinite transparent sort center",
            ));
        }
        Ok(depth)
    }
    pub fn new(mesh: Mesh, material: MaterialHandle) -> Self {
        Self {
            mesh,
            material,
            transform: None,
            sort_center: [0.; 3],
            tint: [1.; 4],
            casts_shadow: true,
        }
    }
}
/// Per-view inputs. Draws may freely mix material families in submission order.
pub struct Frame<'a> {
    pub view: &'a PreparedView,
    pub defaults: DefaultViews,
    pub draws: &'a [Draw],
    pub lights: &'a [PointLight],
    pub camera: Option<BufferHandle>,
}
impl<'a> Frame<'a> {
    pub fn new(view: &'a PreparedView, defaults: DefaultViews, draws: &'a [Draw]) -> Self {
        Self {
            view,
            defaults,
            draws,
            lights: &[],
            camera: None,
        }
    }
}

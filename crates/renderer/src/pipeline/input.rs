use super::{DefaultViews, LocalLight, MaterialHandle, PreparedView, Transform};
use renderer_core::graph::{BufferHandle, GraphError};

/// A graph-local indexed mesh range. Vertex records must match the material's program.
#[derive(Clone, Copy, Debug)]
pub struct Mesh {
    /// Bounds of this indexed range in the same space as its vertices.
    pub bounds: super::Bounds,
    pub vertex_stride: u32,
    pub vertices: BufferHandle,
    pub indices: BufferHandle,
    pub first_index: u32,
    pub index_count: u32,
}
#[derive(Clone, Copy, Debug)]
pub struct Draw {
    pub lightmap: Option<crate::pbr::DrawLightmap>,
    pub mesh: Mesh,
    pub material: MaterialHandle,
    /// None selects world-space vertex records.
    pub transform: Option<Transform>,
    /// Local-space point used for transparent sorting. Defaults to the center of the indexed range.
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
    /// Conservative world-space box. Overflow leaves the draw eligible for all views.
    pub(super) fn world_bounds(
        &self,
        material: &crate::material::MaterialInstance,
    ) -> Option<super::Bounds> {
        // A custom vertex program can move vertices outside the mesh bounds.
        if matches!(material, crate::material::MaterialInstance::Custom(_)) {
            return None;
        }
        let Some(transform) = self.transform else {
            return Some(self.mesh.bounds);
        };
        let m = transform.columns();
        let min = self.mesh.bounds.min().map(f64::from);
        let max = self.mesh.bounds.max().map(f64::from);
        let mut lower = [0.; 3];
        let mut upper = [0.; 3];
        for row in 0..3 {
            let translation = f64::from(m[3][row]);
            let mut lo = translation;
            let mut hi = translation;
            let mut magnitude = translation.abs();
            for col in 0..3 {
                let a = f64::from(m[col][row]) * min[col];
                let b = f64::from(m[col][row]) * max[col];
                lo += a.min(b);
                hi += a.max(b);
                magnitude += a.abs().max(b.abs());
            }
            let margin = magnitude * 8. * f64::from(f32::EPSILON) + f64::from(f32::MIN_POSITIVE);
            lower[row] = ((lo - margin) as f32).next_down();
            upper[row] = ((hi + margin) as f32).next_up();
        }
        super::Bounds::new(lower, upper).ok()
    }

    pub fn new(mesh: Mesh, material: MaterialHandle) -> Self {
        Self {
            mesh,
            lightmap: None,
            material,
            transform: None,
            sort_center: mesh.bounds.center(),
            tint: [1.; 4],
            casts_shadow: true,
        }
    }
}
/// Per-view inputs. Draws may freely mix material families in submission order.
pub struct GeometryDrawInput<'a> {
    pub view: &'a PreparedView,
    pub defaults: DefaultViews,
    pub draws: &'a [Draw],
    pub lights: &'a [LocalLight],
    pub camera: Option<BufferHandle>,
    pub(crate) rect_target: Option<crate::pbr::rect_shadows::Targets<'a>>,
    /// Cascade decisions from the history transaction for this view.
    pub shadow_history: Option<&'a crate::pbr::ShadowFrame>,
}
impl<'a> GeometryDrawInput<'a> {
    /// Use a rectangle atlas selected for this view and Graph.
    pub fn use_rect_shadow_history(&mut self, history: &crate::pbr::RectShadowFrame<'a>) {
        self.rect_target = Some(history.targets);
    }

    pub fn new(view: &'a PreparedView, defaults: DefaultViews, draws: &'a [Draw]) -> Self {
        Self {
            view,
            defaults,
            draws,
            lights: &[],
            camera: None,
            shadow_history: None,
            rect_target: None,
        }
    }
}

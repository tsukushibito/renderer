use super::Bounds;

/// Conservative world-space AABB tests for a PBR camera and its four cascades.
#[derive(Clone, Copy, Debug)]
pub struct ViewCulling {
    frustum: Frustum,
    cascades: Option<[CascadeClip; 4]>,
}

impl ViewCulling {
    /// Prepare the tests from the column-major matrices consumed by the
    /// shaders and the world bounds used to establish the error margins.
    pub fn new(
        matrix: [[f32; 4]; 4],
        cascade_matrices: Option<&[[[f32; 4]; 4]; 4]>,
        scene: Bounds,
    ) -> Self {
        Self {
            frustum: Frustum::new(matrix, scene),
            cascades: cascade_matrices
                .map(|matrices| matrices.map(|matrix| CascadeClip::new(&matrix, scene))),
        }
    }

    /// Return whether the camera may see geometry in the supplied bounds.
    pub fn visible(&self, bounds: Bounds) -> bool {
        self.frustum.overlaps(bounds)
    }

    /// Return whether the geometry may overlap the cascade's shadow map.
    /// Returns `None` for an index outside the prepared maps.
    pub fn cascade_overlaps(&self, cascade: usize, bounds: Bounds) -> Option<bool> {
        self.cascades
            .as_ref()?
            .get(cascade)
            .map(|clip| clip.overlaps(bounds))
    }
}

/// Clip planes using the exact FP32 view-projection matrix consumed by PBR.
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    scene: Bounds,
    planes: Option<[ClipPlane; 6]>,
}

impl Frustum {
    /// Prepare planes for a column-major clip transform with depth in `[0,W]`.
    pub fn new(matrix: [[f32; 4]; 4], scene: Bounds) -> Self {
        let rows = to_rows(&matrix);
        let limits = position_limits(scene);
        let [Some(dx), Some(dy), Some(dz), Some(dw)] =
            rows.map(|row| error_bound(row, limits, 32.0))
        else {
            return Self {
                scene,
                planes: None,
            };
        };
        let [x, y, z, w] = rows;
        Self {
            scene,
            // Work in homogeneous coordinates. Flipping clip Y exchanges the
            // two Y planes and leaves the test unchanged.
            planes: Some([
                ClipPlane::combine(w, x, 1.0, dw + dx),
                ClipPlane::combine(w, x, -1.0, dw + dx),
                ClipPlane::combine(w, y, 1.0, dw + dy),
                ClipPlane::combine(w, y, -1.0, dw + dy),
                ClipPlane {
                    coefficients: z,
                    tolerance: dz,
                },
                ClipPlane::combine(w, z, -1.0, dw + dz),
            ]),
        }
    }

    /// Retain a candidate if it touches a clip plane, exceeds the prepared
    /// scene bounds or the matrix's error bound could not be established.
    pub fn overlaps(&self, bounds: Bounds) -> bool {
        !contains(self.scene, bounds)
            || self
                .planes
                .as_ref()
                .is_none_or(|planes| planes.iter().all(|plane| plane.overlaps(bounds)))
    }
}

#[derive(Clone, Copy, Debug)]
struct ClipPlane {
    coefficients: [f64; 4],
    tolerance: f64,
}

impl ClipPlane {
    fn combine(row: [f64; 4], other: [f64; 4], sign: f64, tolerance: f64) -> Self {
        Self {
            coefficients: std::array::from_fn(|i| row[i] + sign * other[i]),
            tolerance,
        }
    }

    fn overlaps(&self, bounds: Bounds) -> bool {
        let min = bounds.min();
        let max = bounds.max();
        let mut maximum = self.coefficients[3];
        for (axis, coefficient) in self.coefficients[..3].iter().copied().enumerate() {
            let corner = if coefficient >= 0.0 {
                max[axis]
            } else {
                min[axis]
            };
            maximum += coefficient * f64::from(corner);
        }
        maximum >= -self.tolerance || !maximum.is_finite()
    }
}

/// Conservative test against the XY extent of an orthographic shadow map.
#[derive(Clone, Copy, Debug)]
pub struct CascadeClip {
    scene: Bounds,
    axes: Option<[ClipAxis; 2]>,
}

impl CascadeClip {
    /// Prepare from a column-major transform whose homogeneous W is 1.
    /// Other projections retain all candidates.
    pub fn new(matrix: &[[f32; 4]; 4], scene: Bounds) -> Self {
        let rows = to_rows(matrix);
        let limits = position_limits(scene);
        let axes = match rows {
            [x, y, _, [0.0, 0.0, 0.0, 1.0]] => ClipAxis::new(x, limits)
                .zip(ClipAxis::new(y, limits))
                .map(|(x, y)| [x, y]),
            _ => None,
        };
        Self { scene, axes }
    }

    /// Retain candidates inside the map's XY extent at any depth. Bounds
    /// outside the prepared scene are also retained.
    pub fn overlaps(&self, bounds: Bounds) -> bool {
        !contains(self.scene, bounds)
            || self
                .axes
                .as_ref()
                .is_none_or(|axes| axes.iter().all(|axis| axis.overlaps(bounds)))
    }
}

#[derive(Clone, Copy, Debug)]
struct ClipAxis {
    row: [f64; 4],
    limit: f64,
}

impl ClipAxis {
    fn new(row: [f64; 4], position_limits: [f64; 3]) -> Option<Self> {
        Some(Self {
            row,
            limit: 1.0 + error_bound(row, position_limits, 8.0)?,
        })
    }

    fn overlaps(&self, bounds: Bounds) -> bool {
        let min = bounds.min();
        let max = bounds.max();
        let mut minimum = self.row[3];
        let mut maximum = self.row[3];
        for (axis, coefficient) in self.row[..3].iter().enumerate() {
            let a = coefficient * f64::from(min[axis]);
            let b = coefficient * f64::from(max[axis]);
            minimum += a.min(b);
            maximum += a.max(b);
        }
        minimum <= self.limit && maximum >= -self.limit
    }
}

fn to_rows(matrix: &[[f32; 4]; 4]) -> [[f64; 4]; 4] {
    std::array::from_fn(|row| matrix.map(|column| f64::from(column[row])))
}

fn position_limits(bounds: Bounds) -> [f64; 3] {
    let min = bounds.min();
    let max = bounds.max();
    std::array::from_fn(|axis| f64::from(min[axis].abs().max(max[axis].abs())))
}

fn contains(scene: Bounds, bounds: Bounds) -> bool {
    let scene_min = scene.min();
    let scene_max = scene.max();
    let min = bounds.min();
    let max = bounds.max();
    (0..3).all(|axis| scene_min[axis] <= min[axis] && max[axis] <= scene_max[axis])
}

fn error_bound(row: [f64; 4], position_limits: [f64; 3], slack: f64) -> Option<f64> {
    let size = row[3].abs()
        + row[..3]
            .iter()
            .zip(position_limits)
            .map(|(coefficient, limit)| coefficient.abs() * limit)
            .sum::<f64>();
    if !size.is_finite() || size >= 0.5 * f64::from(f32::MAX) {
        return None;
    }
    // Bound the shader's FP32 operations against the sum of their absolute
    // terms. Include cancellation and flushing of subnormal values.
    let rounding = slack * f64::from(f32::EPSILON) * size.max(1.0);
    let denorm = 8.0
        * f64::from(f32::MIN_POSITIVE)
        * (1.0
            + position_limits.into_iter().sum::<f64>()
            + row.into_iter().map(f64::abs).sum::<f64>());
    Some(rounding + denorm)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    fn point(value: [f32; 3]) -> Bounds {
        Bounds::new(value, value).unwrap()
    }

    #[test]
    fn camera_rejects_only_certified_outside_bounds() {
        let scene = Bounds::new([-4.0; 3], [4.0; 3]).unwrap();
        let camera = Frustum::new(IDENTITY, scene);
        assert!(camera.overlaps(point([1.0, 0.0, 0.0])));
        assert!(!camera.overlaps(point([2.0, 0.0, 0.0])));
        assert!(!camera.overlaps(point([0.0, 0.0, -0.5])));
        assert!(camera.overlaps(point([8.0, 0.0, 0.0])));
        let mut overflow = IDENTITY;
        overflow[0][0] = f32::MAX;
        assert!(Frustum::new(overflow, scene).overlaps(point([2.0, 0.0, 0.0])));
    }

    #[test]
    fn shadow_tests_retain_offscreen_casters_and_uncertain_projections() {
        let scene = Bounds::new([-4.0; 3], [4.0; 3]).unwrap();
        let mut perspective = IDENTITY;
        perspective[2][3] = 1.0;
        let view = ViewCulling::new(
            IDENTITY,
            Some(&[IDENTITY, IDENTITY, IDENTITY, perspective]),
            scene,
        );
        let caster = point([1.0, 0.0, -3.0]);
        assert!(!view.visible(caster));
        assert_eq!(view.cascade_overlaps(0, caster), Some(true));
        let outside = point([2.0, 0.0, -3.0]);
        assert_eq!(view.cascade_overlaps(0, outside), Some(false));
        assert_eq!(view.cascade_overlaps(0, point([8.0, 0.0, 0.0])), Some(true));
        assert_eq!(view.cascade_overlaps(3, outside), Some(true));
        assert_eq!(view.cascade_overlaps(4, outside), None);
    }
}

//! Validated application-owned view and projection data.

use renderer::graph::GraphError;

/// How view rays behave in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Projection {
    /// A perspective frustum, including asymmetric projections.
    Perspective,
    /// A centered perspective frustum using the stable analytic shadow fit.
    SymmetricPerspective {
        vertical_fov_degrees: f32,
        aspect: f32,
    },
    /// Parallel view rays with linear depth.
    Orthographic,
}

/// The application-owned view used for raster, visibility and lighting.
///
/// Construct a replacement when the application's camera changes. The matrix
/// is column-major and maps world coordinates into the Renderer's top-origin
/// clip space with depth `0..=w`, near at zero, and positive `w` inside the
/// frustum. `forward` points from the eye into the scene. The near and far
/// planes are perpendicular to this direction and lie at the given positive
/// world-space distances from the eye.
///
/// Either camera handedness is accepted. Standard PBR treats counterclockwise
/// triangles in the raster target as front-facing. An application that changes
/// coordinate conventions must supply winding and tangent handedness that
/// match its geometry. Each shadow cascade determines facing in its own
/// projection.
#[derive(Clone, Copy, Debug)]
pub struct ViewInput {
    pub(super) matrix: [[f32; 4]; 4],
    pub(super) eye: [f32; 3],
    pub(super) forward: [f64; 3],
    pub(super) near_far: [f32; 2],
    pub(super) shape: Shape,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Shape {
    Symmetric(f64),
    Perspective([[f64; 3]; 4]),
    Orthographic([[f64; 3]; 4]),
}

impl ViewInput {
    pub(crate) fn sort_direction(self) -> [f64; 3] {
        self.forward
    }
    /// Validate the projection and retain the exact FP32 raster matrix.
    ///
    /// Off-axis and oblique *orientations* of the whole view are allowed. The
    /// near/far clip planes remain perpendicular to the view direction.
    pub fn new(
        clip_from_world: [[f32; 4]; 4],
        eye: [f32; 3],
        forward: [f64; 3],
        [near, far]: [f32; 2],
        projection: Projection,
    ) -> Result<Self, GraphError> {
        if !near.is_finite()
            || near <= 0.0
            || !far.is_finite()
            || far <= near
            || clip_from_world
                .as_flattened()
                .iter()
                .chain(&eye)
                .any(|value| !value.is_finite())
        {
            return Err(invalid_view());
        }
        let length = dot(forward, forward).sqrt();
        if !length.is_finite() || length < 1e-12 {
            return Err(invalid_view());
        }
        let forward = forward.map(|component| component / length);
        let eye64 = eye.map(f64::from);
        let [x, y, z, w] = std::array::from_fn(|row| {
            let [a, b, c, d] = clip_from_world.map(|column| f64::from(column[row]));
            Plane {
                normal: [a, b, c],
                constant: d,
            }
        });
        let w_scale = w.slope(forward)?;
        let orthographic = projection == Projection::Orthographic;
        if orthographic {
            if w_scale != 0.0 || w.constant <= 0.0 {
                return Err(invalid_view());
            }
        } else if w_scale <= 0.0 || [x, y, w].into_iter().any(|plane| !plane.contains(eye64)) {
            return Err(invalid_view());
        }
        let near_point = add_scaled(eye64, forward, f64::from(near));
        let far_point = add_scaled(eye64, forward, f64::from(far));
        if z.slope(forward)? <= w_scale
            || !z.contains(near_point)
            || (z.at(far_point) - w.at(far_point)).abs()
                > PROJECTION_ROUNDING * (z.scale(far_point) + w.scale(far_point))
        {
            return Err(invalid_view());
        }

        // Dual basis for solving the clip-X/Y equations within the plane
        // perpendicular to `forward`, including sheared projections. The
        // signed divisor is needed for the inverse under either orientation.
        let right = cross(y.normal, forward);
        let up = cross(forward, x.normal);
        let divisor = dot(x.normal, right);
        if divisor.abs()
            <= PROJECTION_ROUNDING * (dot(x.normal, x.normal) * dot(y.normal, y.normal)).sqrt()
        {
            return Err(invalid_view());
        }
        let shape = match projection {
            Projection::SymmetricPerspective {
                vertical_fov_degrees,
                aspect,
            } => {
                if !vertical_fov_degrees.is_finite()
                    || vertical_fov_degrees <= 0.0
                    || vertical_fov_degrees >= 179.0
                    || !aspect.is_finite()
                    || aspect <= 0.0
                {
                    return Err(invalid_view());
                }
                let tangent = (f64::from(vertical_fov_degrees).to_radians() * 0.5).tan();
                let scale = w_scale / tangent;
                if !same_scale(dot(x.normal, x.normal), (scale / f64::from(aspect)).powi(2))
                    || !same_scale(dot(y.normal, y.normal), scale * scale)
                    || !perpendicular(x.normal, y.normal)
                    || !perpendicular(x.normal, forward)
                    || !perpendicular(y.normal, forward)
                {
                    return Err(invalid_view());
                }
                Shape::Symmetric(tangent * tangent * (1.0 + f64::from(aspect).powi(2)))
            }
            Projection::Perspective | Projection::Orthographic => {
                let (u, v, width) = if orthographic {
                    (x.at(eye64), y.at(eye64), w.constant)
                } else {
                    (dot(x.normal, forward), dot(y.normal, forward), w_scale)
                };
                let corners = std::array::from_fn(|corner| {
                    let horizontal = if corner & 1 == 0 { -width } else { width };
                    let vertical = if corner & 2 == 0 { -width } else { width };
                    std::array::from_fn(|axis| {
                        ((horizontal - u) * right[axis] + (vertical - v) * up[axis]) / divisor
                    })
                });
                if corners.as_flattened().iter().any(|x| !x.is_finite()) {
                    return Err(invalid_view());
                }
                if orthographic {
                    Shape::Orthographic(corners)
                } else {
                    Shape::Perspective(corners)
                }
            }
        };
        Ok(Self {
            matrix: clip_from_world,
            eye,
            forward,
            near_far: [near, far],
            shape,
        })
    }
}

const PROJECTION_ROUNDING: f64 = 32.0 * f32::EPSILON as f64;

#[derive(Clone, Copy)]
struct Plane {
    normal: [f64; 3],
    constant: f64,
}

impl Plane {
    fn at(self, point: [f64; 3]) -> f64 {
        dot(self.normal, point) + self.constant
    }

    fn contains(self, point: [f64; 3]) -> bool {
        self.at(point).abs() <= PROJECTION_ROUNDING * self.scale(point)
    }

    fn scale(self, point: [f64; 3]) -> f64 {
        self.constant.abs()
            + self
                .normal
                .into_iter()
                .zip(point)
                .map(|(normal, coordinate)| (normal * coordinate).abs())
                .sum::<f64>()
    }

    fn slope(self, direction: [f64; 3]) -> Result<f64, GraphError> {
        let slope = dot(self.normal, direction);
        let residual = add_scaled(self.normal, direction, -slope);
        let magnitude = dot(self.normal, self.normal).sqrt();
        if dot(residual, residual).sqrt() > PROJECTION_ROUNDING * magnitude {
            return Err(invalid_view());
        }
        Ok(slope)
    }
}

fn same_scale(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= PROJECTION_ROUNDING * expected
}

fn perpendicular(first: [f64; 3], second: [f64; 3]) -> bool {
    dot(first, second).abs()
        <= PROJECTION_ROUNDING * (dot(first, first) * dot(second, second)).sqrt()
}

pub(super) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}

pub(super) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(super) fn add_scaled(point: [f64; 3], direction: [f64; 3], scale: f64) -> [f64; 3] {
    std::array::from_fn(|axis| point[axis] + direction[axis] * scale)
}

fn invalid_view() -> GraphError {
    GraphError::invalid_input("invalid PBR view matrix, projection kind, or camera geometry")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_axis_perspective_maps_unprojected_rays_to_the_clip_edges() {
        let matrix = [
            [1.5, 0.0, 0.0, 0.0],
            [0.0, -2.0, 0.0, 0.0],
            [0.25, -0.125, -1.125, -1.0],
            [0.0, 0.0, -1.125, 0.0],
        ];
        for z_axis in [1.0, -1.0] {
            let mut matrix = matrix;
            matrix[2] = matrix[2].map(|word| word * z_axis);
            let input = ViewInput::new(
                matrix,
                [0.0; 3],
                [0.0, 0.0, -f64::from(z_axis)],
                [1.0, 9.0],
                Projection::Perspective,
            )
            .unwrap();
            let Shape::Perspective(rays) = input.shape else {
                panic!("expected perspective rays")
            };
            for distance in [1.0, 4.0, 9.0] {
                for (ray, [x, y]) in
                    rays.into_iter()
                        .zip([[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]])
                {
                    let relative =
                        add_scaled(input.forward, ray, 1.0).map(|coordinate| coordinate * distance);
                    let project = |row| {
                        f64::from(matrix[3][row])
                            + (0..3)
                                .map(|column| f64::from(matrix[column][row]) * relative[column])
                                .sum::<f64>()
                    };
                    assert!((project(0) / project(3) - x).abs() < 1e-12);
                    assert!((project(1) / project(3) - y).abs() < 1e-12);
                }
            }
        }
        for (kind, planes) in [
            (Projection::Orthographic, [1.0, 9.0]),
            (Projection::Perspective, [1.0, 10.0]),
            (
                Projection::SymmetricPerspective {
                    vertical_fov_degrees: 60.0,
                    aspect: 1.0,
                },
                [1.0, 9.0],
            ),
        ] {
            assert!(ViewInput::new(matrix, [0.0; 3], [0.0, 0.0, -1.0], planes, kind).is_err());
        }
    }

    #[test]
    fn symmetric_perspective_accepts_either_forward_axis() {
        for forward_z in [-1.0, 1.0] {
            assert!(
                ViewInput::new(
                    [
                        [1.0, 0.0, 0.0, 0.0],
                        [0.0, -1.0, 0.0, 0.0],
                        [0.0, 0.0, 1.125 * forward_z, forward_z],
                        [0.0, 0.0, -1.125, 0.0],
                    ],
                    [0.0; 3],
                    [0.0, 0.0, f64::from(forward_z)],
                    [1.0, 9.0],
                    Projection::SymmetricPerspective {
                        vertical_fov_degrees: 90.0,
                        aspect: 1.0,
                    },
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn orthographic_view_accepts_both_orientations_and_rejects_a_collapsed_basis() {
        for forward_z in [-1.0, 1.0] {
            let mut matrix = [
                [2.0, 0.0, 0.0, 0.0],
                [0.0, -4.0, 0.0, 0.0],
                [0.0, 0.0, 0.125 * forward_z, 0.0],
                [0.25, -0.5, -0.125, 1.0],
            ];
            let prepare = |matrix| {
                ViewInput::new(
                    matrix,
                    [0.0; 3],
                    [0.0, 0.0, f64::from(forward_z)],
                    [1.0, 9.0],
                    Projection::Orthographic,
                )
            };
            let Shape::Orthographic(corners) = prepare(matrix).unwrap().shape else {
                panic!("expected orthographic offsets")
            };
            assert_eq!(
                corners,
                [
                    [-0.625, 0.125, 0.0],
                    [0.375, 0.125, 0.0],
                    [-0.625, -0.375, 0.0],
                    [0.375, -0.375, 0.0],
                ]
            );
            matrix[1][1] = 0.0;
            assert!(prepare(matrix).is_err());
        }
    }
}

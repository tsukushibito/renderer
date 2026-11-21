//! Stable world-space cameras for the four directional shadow images.

use renderer::graph::GraphError;

use super::{
    ViewInput,
    input::{Shape, add_scaled, cross, dot},
};

const CASCADE_COUNT: usize = 4;
const PCF_MARGIN_TEXELS: f64 = 2.0;

/// Finite world-space bounds enclosing the shadow casters. Flat and point-sized
/// bounds are permitted.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    min: [f32; 3],
    max: [f32; 3],
}

impl Bounds {
    pub fn new(min: [f32; 3], max: [f32; 3]) -> Result<Self, GraphError> {
        if min
            .into_iter()
            .zip(max)
            .any(|(a, b)| !a.is_finite() || !b.is_finite() || a > b)
        {
            return Err(GraphError::invalid_input("invalid PBR shadow bounds"));
        }
        Ok(Self { min, max })
    }

    pub fn min(self) -> [f32; 3] {
        self.min
    }

    pub fn max(self) -> [f32; 3] {
        self.max
    }

    /// Midpoint of the finite AABB, including bounds near the limits of f32.
    pub fn center(self) -> [f32; 3] {
        std::array::from_fn(|i| self.min[i].midpoint(self.max[i]))
    }

    fn corners(self) -> [[f64; 3]; 8] {
        std::array::from_fn(|index| {
            std::array::from_fn(|axis| {
                f64::from(if index & (1 << axis) == 0 {
                    self.min[axis]
                } else {
                    self.max[axis]
                })
            })
        })
    }
}

/// Four-cascade directional shadow settings. Bias and receiver normal offset
/// are measured in world-space texels at the current cascade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowSettings {
    pub resolution: u32,
    pub distance: f32,
    pub split_lambda: f32,
    pub blend_fraction: f32,
    pub depth_bias_texels: f32,
    pub slope_bias_texels: f32,
    pub normal_offset_texels: f32,
    /// World-space padding on both sides of the casters' light-space depth.
    pub depth_padding: f32,
}

impl Default for ShadowSettings {
    fn default() -> Self {
        Self {
            resolution: 2048,
            distance: 120.0,
            split_lambda: 0.6,
            blend_fraction: 0.1,
            depth_bias_texels: 0.75,
            slope_bias_texels: 1.5,
            normal_offset_texels: 1.0,
            depth_padding: 1.0,
        }
    }
}

impl ShadowSettings {
    pub(crate) fn validate(&self) -> Result<(), GraphError> {
        if self.resolution < 8
            || !self.distance.is_finite()
            || self.distance <= 0.0
            || !(0.0..=1.0).contains(&self.split_lambda)
            || !(0.0..1.0).contains(&self.blend_fraction)
            || [
                self.depth_bias_texels,
                self.slope_bias_texels,
                self.normal_offset_texels,
            ]
            .into_iter()
            .any(|value| !value.is_finite() || value < 0.0)
            || !self.depth_padding.is_finite()
            || self.depth_padding <= 0.0
            || f64::from(self.resolution)
                <= 2.0 * (PCF_MARGIN_TEXELS + f64::from(self.normal_offset_texels))
        {
            return Err(GraphError::invalid_input("invalid PBR shadow settings"));
        }
        Ok(())
    }
}

/// The 336-byte appendage of the camera uniform, in shader field order.
#[derive(Clone, Copy, Debug)]
pub(super) struct ShadowData {
    pub(super) matrices: [[[f32; 4]; 4]; CASCADE_COUNT],
    splits: [f32; CASCADE_COUNT],
    texel_world: [f32; CASCADE_COUNT],
    depth_scale: [f32; CASCADE_COUNT],
    camera_forward_blend: [f32; 4],
    bias: [f32; 4],
}

impl ShadowData {
    pub(super) fn words(&self) -> impl Iterator<Item = &f32> {
        self.matrices.as_flattened().as_flattened().iter().chain(
            self.splits
                .iter()
                .chain(&self.texel_world)
                .chain(&self.depth_scale)
                .chain(&self.camera_forward_blend)
                .chain(&self.bias),
        )
    }
}

fn normalize(a: [f64; 3]) -> Result<[f64; 3], GraphError> {
    let length = dot(a, a).sqrt();
    if !length.is_finite() || length < 1e-12 {
        return Err(GraphError::invalid_input(
            "degenerate shadow camera or light direction",
        ));
    }
    Ok(a.map(|component| component / length))
}

fn split_near(splits: [f32; CASCADE_COUNT], near: f64, blend: f64, index: usize) -> f64 {
    if index == 0 {
        near
    } else {
        let previous_end = f64::from(splits[index - 1]);
        let previous_start = if index == 1 {
            near
        } else {
            f64::from(splits[index - 2])
        };
        // The preceding cascade's last blend interval also samples this map.
        previous_end - blend * (previous_end - previous_start)
    }
}

fn slice_sphere(view: ViewInput, start: f64, end: f64) -> ([f64; 3], f64) {
    let position = view.eye.map(f64::from);
    let (corners, perspective) = match view.shape {
        Shape::Symmetric(corner_slope_squared) => {
            // Retain the stable analytic sphere for symmetric perspectives.
            let center_distance = ((start + end) * (1.0 + corner_slope_squared) * 0.5).min(end);
            let radius =
                ((end - center_distance).powi(2) + corner_slope_squared * end * end).sqrt();
            return (add_scaled(position, view.forward, center_distance), radius);
        }
        Shape::Perspective(corners) => (corners, true),
        Shape::Orthographic(corners) => (corners, false),
    };
    let points = [start, end].map(|distance| {
        let center = view.forward.map(|component| component * distance);
        let scale = if perspective { distance } else { 1.0 };
        corners.map(|corner| add_scaled(center, corner, scale))
    });
    let points = points.as_flattened();
    let center = std::array::from_fn(|axis| points.iter().map(|p| p[axis]).sum::<f64>() / 8.0);
    let radius_squared = points
        .iter()
        .map(|&point| {
            let ray = add_scaled(point, center, -1.0);
            dot(ray, ray)
        })
        .fold(0.0, f64::max);
    (add_scaled(position, center, 1.0), radius_squared.sqrt())
}

/// Calculate four immutable shadow cameras using only stack-sized workspaces.
///
/// `direction_to_light` has the same meaning as the PBR sun vector. Geometry
/// closest to the light receives the smallest shadow depth. The last split is
/// min(main far, shadow distance); no shadow is implied beyond that distance.
pub(super) fn calculate(
    view: ViewInput,
    direction_to_light: [f32; 3],
    bounds: Bounds,
    settings: ShadowSettings,
) -> Result<ShadowData, GraphError> {
    let [near, far] = view.near_far;
    if settings.distance <= near {
        return Err(GraphError::invalid_input(
            "shadow distance must exceed the camera near plane",
        ));
    }
    let light_z = normalize(direction_to_light.map(f64::from))?;
    let reference_up = if light_z[1].abs() < 0.99 {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    let light_x = normalize(cross(reference_up, light_z))?;
    let light_y = cross(light_z, light_x);
    let mut minimum_z = f64::INFINITY;
    let mut maximum_z = f64::NEG_INFINITY;
    for corner in bounds.corners() {
        let z = dot(light_z, corner);
        minimum_z = minimum_z.min(z);
        maximum_z = maximum_z.max(z);
    }
    minimum_z -= f64::from(settings.depth_padding);
    maximum_z += f64::from(settings.depth_padding);
    let inverse_depth = 1.0 / (maximum_z - minimum_z);
    let near = f64::from(near);
    let end = f64::from(far.min(settings.distance));
    let lambda = f64::from(settings.split_lambda);
    let mut splits = [0.0; CASCADE_COUNT];
    for (index, split) in splits.iter_mut().enumerate() {
        let fraction = (index + 1) as f64 / CASCADE_COUNT as f64;
        let logarithmic = near * (end / near).powf(fraction);
        let uniform = near + (end - near) * fraction;
        *split = if index + 1 == CASCADE_COUNT {
            end as f32
        } else {
            (lambda * logarithmic + (1.0 - lambda) * uniform) as f32
        };
    }
    let mut previous = near;
    for split in splits {
        if f64::from(split) <= previous {
            return Err(GraphError::invalid_input(
                "shadow split intervals collapse at FP32 precision",
            ));
        }
        previous = f64::from(split);
    }
    let resolution = f64::from(settings.resolution);
    let mut output = ShadowData {
        matrices: [[[0.0; 4]; 4]; CASCADE_COUNT],
        splits,
        texel_world: [0.0; CASCADE_COUNT],
        depth_scale: [inverse_depth as f32; CASCADE_COUNT],
        camera_forward_blend: [
            view.forward[0] as f32,
            view.forward[1] as f32,
            view.forward[2] as f32,
            settings.blend_fraction,
        ],
        bias: [
            settings.depth_bias_texels,
            settings.slope_bias_texels,
            settings.normal_offset_texels,
            0.0,
        ],
    };
    for index in 0..CASCADE_COUNT {
        let start = split_near(splits, near, f64::from(settings.blend_fraction), index);
        let end = f64::from(splits[index]);
        let (center, radius) = slice_sphere(view, start, end);
        // Two texels cover one PCF tap plus the half-texel center snap and leave
        // room at the image edge. The receiver's geometric-normal offset may
        // also move in XY, so reserve its maximum displacement before solving
        // the margin in the final texel units.
        let margin = PCF_MARGIN_TEXELS + f64::from(settings.normal_offset_texels);
        let half_extent = radius / (1.0 - 2.0 * margin / resolution);
        let texel = 2.0 * half_extent / resolution;
        let center_x = (dot(light_x, center) / texel).round() * texel;
        let center_y = (dot(light_y, center) / texel).round() * texel;
        let inverse_xy = 1.0 / half_extent;
        for column in 0..3 {
            output.matrices[index][column] = [
                (light_x[column] * inverse_xy) as f32,
                (-light_y[column] * inverse_xy) as f32,
                (-light_z[column] * inverse_depth) as f32,
                0.0,
            ];
        }
        output.matrices[index][3] = [
            (-center_x * inverse_xy) as f32,
            (center_y * inverse_xy) as f32,
            (maximum_z * inverse_depth) as f32,
            1.0,
        ];
        output.texel_world[index] = texel as f32;
    }
    if output
        .matrices
        .as_flattened()
        .as_flattened()
        .iter()
        .chain(&output.texel_world)
        .chain(&output.depth_scale)
        .any(|value| !value.is_finite())
        || output.texel_world.into_iter().any(|value| value <= 0.0)
        || output.depth_scale[0] <= 0.0
    {
        return Err(GraphError::invalid_input(
            "shadow projection exceeds finite FP32 range",
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_depth_retains_offscreen_casters_and_grows_away_from_the_sun() {
        let view = super::super::test_perspective([1.0, 100.0]);
        let bounds = Bounds::new([-300.0, -100.0, -300.0], [300.0, 150.0, 300.0]).unwrap();
        let result = calculate(
            view,
            [0.0, 0.0, 1.0],
            bounds,
            ShadowSettings {
                split_lambda: 0.5,
                ..Default::default()
            },
        )
        .unwrap();
        for (actual, expected) in result
            .splits
            .into_iter()
            .zip([14.456139, 30.25, 53.43639, 100.0])
        {
            assert!((actual - expected).abs() < 1e-5);
        }
        for matrix in result.matrices {
            let depth = |z: f32| matrix[2][2] * z + matrix[3][2];
            assert!(0.0 < depth(300.0) && depth(300.0) < depth(-300.0) && depth(-300.0) < 1.0);
        }
    }
}

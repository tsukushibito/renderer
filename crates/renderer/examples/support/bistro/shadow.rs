//! Consumer-owned, allocation-free cascaded sun-shadow camera calculations.
//!
//! Matrices use the same column-major, negative clip-Y and normal [0, 1] depth
//! convention as `camera`. The light basis and depth extent do not follow the
//! main camera: offscreen geometry in the scene bounds remains a valid caster.

use super::{Error, camera::Camera};

pub(super) const CASCADE_COUNT: usize = 4;
pub(super) const UNIFORM_BYTES: usize = 336;
const PCF_MARGIN_TEXELS: f64 = 2.0;

/// Validated once when the resident scene is prepared, not by scanning vertices
/// during an ordinary frame. A flat or point-sized scene is permitted.
#[derive(Clone, Copy, Debug)]
pub(super) struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Bounds {
    pub fn new(min: [f32; 3], max: [f32; 3]) -> Result<Self, Error> {
        if min
            .into_iter()
            .zip(max)
            .any(|(a, b)| !a.is_finite() || !b.is_finite() || a > b)
        {
            return Err("invalid shadow scene bounds".into());
        }
        Ok(Self { min, max })
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

/// Initial benchmark settings. Bias remains explicit so fixture and Bistro
/// calibration can change it without changing shader or native pipeline state.
#[derive(Clone, Copy, Debug)]
pub(super) struct Parameters {
    pub resolution: u32,
    pub distance: f32,
    pub split_lambda: f32,
    pub blend_fraction: f32,
    /// Receiver depth bias, measured in the current cascade's world texels.
    pub depth_bias_texels: f32,
    /// Additional depth bias multiplied by (1 - geometric N dot L).
    pub slope_bias_texels: f32,
    /// Geometric-normal offset multiplied by (1 - geometric N dot L).
    pub normal_offset_texels: f32,
    /// World-space padding beyond the scene's light-space depth extrema.
    pub depth_padding: f32,
}

impl Default for Parameters {
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

/// The 336-byte appendix to the common 128-byte Bistro camera/lighting block.
/// Each field begins at a 16-byte boundary; matrices remain column-major.
#[derive(Clone, Copy, Debug)]
pub(super) struct ShadowData {
    pub matrices: [[[f32; 4]; 4]; CASCADE_COUNT],
    pub splits: [f32; CASCADE_COUNT],
    pub texel_world: [f32; CASCADE_COUNT],
    pub depth_scale: [f32; CASCADE_COUNT],
    pub camera_forward_blend: [f32; 4],
    pub bias: [f32; 4],
}

impl ShadowData {
    pub fn uniform_bytes(&self) -> [u8; UNIFORM_BYTES] {
        let mut output = [0; UNIFORM_BYTES];
        let words = self.matrices.as_flattened().as_flattened().iter().chain(
            self.splits
                .iter()
                .chain(&self.texel_world)
                .chain(&self.depth_scale)
                .chain(&self.camera_forward_blend)
                .chain(&self.bias),
        );
        for (destination, value) in output.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *destination = value.to_le_bytes();
        }
        output
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: [f64; 3]) -> Result<[f64; 3], Error> {
    let length = dot(a, a).sqrt();
    if !length.is_finite() || length < 1e-12 {
        return Err("degenerate shadow camera or light direction".into());
    }
    Ok(a.map(|component| component / length))
}

fn camera_forward(camera: Camera) -> Result<[f64; 3], Error> {
    normalize(std::array::from_fn(|axis| {
        f64::from(camera.target[axis]) - f64::from(camera.position[axis])
    }))
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

/// Calculate four immutable shadow cameras using only stack-sized workspaces.
///
/// `direction_to_light` has the same meaning as the PBR sun vector. Geometry
/// closest to the light receives the smallest shadow depth. The last split is
/// min(main far, shadow distance); no shadow is implied beyond that distance.
#[allow(
    clippy::too_many_arguments,
    reason = "the caller supplies independent scene, camera and light inputs"
)]
pub(super) fn calculate(
    camera: Camera,
    aspect: f32,
    near: f32,
    far: f32,
    direction_to_light: [f32; 3],
    bounds: Bounds,
    parameters: Parameters,
) -> Result<ShadowData, Error> {
    if !aspect.is_finite()
        || aspect <= 0.0
        || !near.is_finite()
        || near <= 0.0
        || !far.is_finite()
        || far <= near
        || !camera.fov.is_finite()
        || !(0.0..179.0).contains(&camera.fov)
        || camera.fov == 0.0
        || camera.position.into_iter().any(|value| !value.is_finite())
        || parameters.resolution < 8
        || !parameters.distance.is_finite()
        || parameters.distance <= near
        || !parameters.split_lambda.is_finite()
        || !(0.0..=1.0).contains(&parameters.split_lambda)
        || !parameters.blend_fraction.is_finite()
        || !(0.0..1.0).contains(&parameters.blend_fraction)
        || [
            parameters.depth_bias_texels,
            parameters.slope_bias_texels,
            parameters.normal_offset_texels,
        ]
        .into_iter()
        .any(|value| !value.is_finite() || value < 0.0)
        || !parameters.depth_padding.is_finite()
        || parameters.depth_padding <= 0.0
        || f64::from(parameters.resolution)
            <= 2.0 * (PCF_MARGIN_TEXELS + f64::from(parameters.normal_offset_texels))
    {
        return Err("invalid shadow parameters or perspective camera".into());
    }
    let forward = camera_forward(camera)?;
    // Match the main camera's world-up basis and reject its degenerate poles.
    normalize(cross(forward, [0.0, 1.0, 0.0]))?;
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
    minimum_z -= f64::from(parameters.depth_padding);
    maximum_z += f64::from(parameters.depth_padding);
    let inverse_depth = 1.0 / (maximum_z - minimum_z);
    let near = f64::from(near);
    let end = f64::from(far.min(parameters.distance));
    let lambda = f64::from(parameters.split_lambda);
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
            return Err("shadow split intervals collapse at FP32 precision".into());
        }
        previous = f64::from(split);
    }
    let position = camera.position.map(f64::from);
    let resolution = f64::from(parameters.resolution);
    let tangent = (f64::from(camera.fov).to_radians() * 0.5).tan();
    let corner_slope_squared = tangent * tangent * (1.0 + f64::from(aspect).powi(2));
    let mut output = ShadowData {
        matrices: [[[0.0; 4]; 4]; CASCADE_COUNT],
        splits,
        texel_world: [0.0; CASCADE_COUNT],
        depth_scale: [inverse_depth as f32; CASCADE_COUNT],
        camera_forward_blend: [
            forward[0] as f32,
            forward[1] as f32,
            forward[2] as f32,
            parameters.blend_fraction,
        ],
        bias: [
            parameters.depth_bias_texels,
            parameters.slope_bias_texels,
            parameters.normal_offset_texels,
            0.0,
        ],
    };
    for index in 0..CASCADE_COUNT {
        let start = split_near(splits, near, f64::from(parameters.blend_fraction), index);
        let end = f64::from(splits[index]);
        // Analytic bounding sphere of this symmetric perspective frustum. Its
        // radius depends on FOV/aspect/splits, never the camera's orientation.
        let center_distance = ((start + end) * (1.0 + corner_slope_squared) * 0.5).min(end);
        let radius = ((end - center_distance).powi(2) + corner_slope_squared * end * end).sqrt();
        // Two texels cover one PCF tap plus the half-texel center snap and leave
        // room at the image edge. The receiver's geometric-normal offset may
        // also move in XY, so reserve its maximum displacement before solving
        // the margin in the final texel units.
        let margin = PCF_MARGIN_TEXELS + f64::from(parameters.normal_offset_texels);
        let half_extent = radius / (1.0 - 2.0 * margin / resolution);
        let texel = 2.0 * half_extent / resolution;
        let center = std::array::from_fn(|axis| position[axis] + forward[axis] * center_distance);
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
        return Err("shadow projection exceeds finite FP32 range".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Bounds {
        Bounds::new([-300.0, -100.0, -300.0], [300.0, 150.0, 300.0]).unwrap()
    }

    fn project(matrix: [[f32; 4]; 4], point: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|row| {
            (0..3)
                .map(|column| f64::from(matrix[column][row]) * point[column])
                .sum::<f64>()
                + f64::from(matrix[3][row])
        })
    }

    fn data(camera: Camera, parameters: Parameters) -> ShadowData {
        calculate(
            camera,
            16.0 / 9.0,
            0.02,
            500.0,
            [0.0, 0.0, 1.0],
            bounds(),
            parameters,
        )
        .unwrap()
    }

    #[test]
    fn practical_splits_blend_linear_and_logarithmic_distances() {
        let result = calculate(
            Camera::fixture(),
            1.0,
            1.0,
            100.0,
            [0.0, 0.0, 1.0],
            bounds(),
            Parameters {
                split_lambda: 0.5,
                ..Parameters::default()
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
        assert_eq!(
            data(Camera::fixture(), Parameters::default()).splits[3],
            120.0
        );
    }

    #[test]
    fn every_fixed_view_and_blend_interval_retains_pcf_margin() {
        let conditions: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../tools/bistro/conditions.json"))
                .unwrap();
        let conditions = super::super::camera::Conditions::parse(&conditions).unwrap();
        let parameters = Parameters::default();
        for (_, camera) in conditions.views {
            let result = calculate(
                camera,
                16.0 / 9.0,
                conditions.near,
                conditions.far,
                conditions.light_direction,
                bounds(),
                parameters,
            )
            .unwrap();
            let forward = camera_forward(camera).unwrap();
            let right = normalize(cross(forward, [0.0, 1.0, 0.0])).unwrap();
            let up = cross(right, forward);
            let tangent = (f64::from(camera.fov).to_radians() * 0.5).tan();
            for index in 0..CASCADE_COUNT {
                let start = split_near(result.splits, f64::from(conditions.near), 0.1, index);
                for distance in [start, f64::from(result.splits[index])] {
                    for x in [-1.0, 1.0] {
                        for y in [-1.0, 1.0] {
                            let point = std::array::from_fn(|axis| {
                                f64::from(camera.position[axis])
                                    + forward[axis] * distance
                                    + right[axis] * x * tangent * distance * (16.0 / 9.0)
                                    + up[axis] * y * tangent * distance
                            });
                            let p = project(result.matrices[index], point);
                            // Even the maximum normal offset leaves at least
                            // 1.5 texels after the center snap, so a one-texel
                            // PCF tap cannot escape the image.
                            let maximum = 1.0 - 3.0 / f64::from(parameters.resolution);
                            let normal_offset = 2.0 * f64::from(parameters.normal_offset_texels)
                                / f64::from(parameters.resolution);
                            assert!(p[0].abs() + normal_offset <= maximum + 2e-6, "x {p:?}");
                            assert!(p[1].abs() + normal_offset <= maximum + 2e-6, "y {p:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn light_depth_includes_offscreen_casters_and_increases_away_from_sun() {
        let scene = bounds();
        let result = data(Camera::fixture(), Parameters::default());
        for matrix in result.matrices {
            for corner in scene.corners() {
                let depth = project(matrix, corner)[2];
                assert!(depth > 0.0 && depth < 1.0);
            }
            let front = project(matrix, [0.0, 0.0, 100.0])[2];
            let back = project(matrix, [0.0, 0.0, -100.0])[2];
            assert!(front < back);
            assert!(project(matrix, [0.0, 1.0, 0.0])[1] < project(matrix, [0.0, 0.0, 0.0])[1]);
        }
    }

    #[test]
    fn subtexel_translation_keeps_shadow_projection_and_rotation_keeps_scale() {
        let camera = Camera {
            position: [0.0; 3],
            target: [0.0, 0.0, -1.0],
            fov: 60.0,
        };
        let original = data(camera, Parameters::default());
        let delta = original.texel_world[0] * 0.25;
        let shifted = data(
            Camera {
                position: [delta, 0.0, 0.0],
                target: [delta, 0.0, -1.0],
                ..camera
            },
            Parameters::default(),
        );
        assert_eq!(original.matrices, shifted.matrices);
        let rotated = data(
            Camera {
                target: [-1.0, 0.0, 0.0],
                ..camera
            },
            Parameters::default(),
        );
        assert_eq!(original.texel_world, rotated.texel_world);
        assert_eq!(original.depth_scale, rotated.depth_scale);
        for (before, after) in original.matrices.into_iter().zip(rotated.matrices) {
            assert_eq!(&before[..3], &after[..3]);
        }
    }

    #[test]
    fn uniform_appendix_preserves_column_order_and_field_offsets() {
        let result = data(Camera::fixture(), Parameters::default());
        let bytes = result.uniform_bytes();
        let word = |offset| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        assert_eq!(bytes.len(), 336);
        assert_eq!(word(0), result.matrices[0][0][0]);
        assert_eq!(word(64 + 12), result.matrices[1][0][3]);
        assert_eq!(word(256), result.splits[0]);
        assert_eq!(word(272), result.texel_world[0]);
        assert_eq!(word(288), result.depth_scale[0]);
        assert_eq!(word(304 + 12), result.camera_forward_blend[3]);
        assert_eq!(word(320 + 8), result.bias[2]);
    }

    #[test]
    fn invalid_settings_and_degenerate_camera_or_light_are_rejected() {
        assert!(Bounds::new([1.0; 3], [0.0; 3]).is_err());
        assert!(Bounds::new([f32::NAN; 3], [1.0; 3]).is_err());
        let camera = Camera::fixture();
        let calculate = |camera, light, parameters| {
            super::calculate(camera, 1.0, 0.02, 500.0, light, bounds(), parameters)
        };
        assert!(calculate(camera, [0.0; 3], Parameters::default()).is_err());
        assert!(
            calculate(
                Camera {
                    target: camera.position,
                    ..camera
                },
                [1.0; 3],
                Parameters::default()
            )
            .is_err()
        );
        assert!(
            calculate(
                camera,
                [1.0; 3],
                Parameters {
                    resolution: 4,
                    ..Parameters::default()
                }
            )
            .is_err()
        );
        assert!(
            calculate(
                camera,
                [1.0; 3],
                Parameters {
                    blend_fraction: 1.0,
                    ..Parameters::default()
                }
            )
            .is_err()
        );
        assert!(
            calculate(
                camera,
                [1.0; 3],
                Parameters {
                    depth_bias_texels: -1.0,
                    ..Parameters::default()
                }
            )
            .is_err()
        );
        assert!(calculate(camera, [0.0, 1.0, 0.0], Parameters::default()).is_ok());
        assert!(Bounds::new([0.0; 3], [0.0; 3]).is_ok());
    }

    #[test]
    fn calibration_settings_cannot_consume_the_projection_or_collapse_splits() {
        assert!(
            calculate(
                Camera::fixture(),
                1.0,
                0.02,
                500.0,
                [1.0; 3],
                bounds(),
                Parameters {
                    resolution: 8,
                    normal_offset_texels: 2.0,
                    ..Parameters::default()
                },
            )
            .is_err()
        );
        assert!(
            calculate(
                Camera::fixture(),
                1.0,
                1.0,
                f32::from_bits(1.0f32.to_bits() + 1),
                [1.0; 3],
                bounds(),
                Parameters::default(),
            )
            .is_err()
        );
    }
}

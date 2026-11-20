//! Typed inputs and host packing for the reviewed Forward shader ABI.

mod shadow;

use crate::passes::pbr::{CAMERA_BYTES, MATERIAL_BYTES};
use renderer::graph::GraphError;

pub use shadow::{Bounds, ShadowSettings};

/// Y-up perspective camera. Angles are in degrees and depth uses [0, 1].
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub fov_y_degrees: f32,
    pub near: f32,
    pub far: f32,
}

impl Camera {
    fn matrix(self, aspect: f32) -> Result<[[f32; 4]; 4], GraphError> {
        if !aspect.is_finite()
            || aspect <= 0.0
            || !self.near.is_finite()
            || self.near <= 0.0
            || !self.far.is_finite()
            || self.far <= self.near
            || !self.fov_y_degrees.is_finite()
            || self.fov_y_degrees <= 0.0
            || self.fov_y_degrees >= 179.0
            || self
                .position
                .into_iter()
                .chain(self.target)
                .any(|v| !v.is_finite())
        {
            return Err(GraphError::invalid_input("invalid PBR perspective camera"));
        }

        let away = std::array::from_fn(|axis| self.position[axis] - self.target[axis]);
        let z = normalize(away, "degenerate PBR camera basis")?;
        let x = normalize(cross([0.0, 1.0, 0.0], z), "degenerate PBR camera basis")?;
        let y = cross(z, x);
        let view = [
            [x[0], y[0], z[0], 0.0],
            [x[1], y[1], z[1], 0.0],
            [x[2], y[2], z[2], 0.0],
            [
                -dot(x, self.position),
                -dot(y, self.position),
                -dot(z, self.position),
                1.0,
            ],
        ];
        let scale = 1.0 / (self.fov_y_degrees.to_radians() * 0.5).tan();
        // Negative clip-Y matches the Vulkan viewport and DXIL vertex flip.
        let projection = [
            [scale / aspect, 0.0, 0.0, 0.0],
            [0.0, -scale, 0.0, 0.0],
            [0.0, 0.0, self.far / (self.near - self.far), -1.0],
            [0.0, 0.0, self.near * self.far / (self.near - self.far), 0.0],
        ];
        let matrix: [[f32; 4]; 4] = std::array::from_fn(|column| {
            std::array::from_fn(|row| (0..4).map(|i| projection[i][row] * view[column][i]).sum())
        });
        if matrix.as_flattened().iter().any(|value| !value.is_finite()) {
            return Err(GraphError::invalid_input(
                "PBR camera matrix exceeds finite FP32 range",
            ));
        }
        Ok(matrix)
    }
}

/// Directional light pointing from the scene toward the sun, with linear RGB.
#[derive(Clone, Copy, Debug)]
pub struct Sun {
    direction: [f32; 3],
    color: [f32; 3],
}

impl Sun {
    pub fn new(direction: [f32; 3], color: [f32; 3]) -> Result<Self, GraphError> {
        Self::from_normalized(normalize(direction, "degenerate PBR sun direction")?, color)
    }

    /// Retain the exact components of an already normalized direction.
    pub fn from_normalized(direction: [f32; 3], color: [f32; 3]) -> Result<Self, GraphError> {
        let length_squared = dot(direction, direction);
        if !length_squared.is_finite() || (length_squared - 1.0).abs() > 8.0 * f32::EPSILON {
            return Err(GraphError::invalid_input(
                "PBR sun direction must be normalized",
            ));
        }
        if color
            .into_iter()
            .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err(GraphError::invalid_input(
                "PBR sun color must be finite and nonnegative",
            ));
        }
        Ok(Self { direction, color })
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(vector: [f32; 3], message: &'static str) -> Result<[f32; 3], GraphError> {
    let length = dot(vector, vector).sqrt();
    if !length.is_finite() || length < 1e-8 {
        return Err(GraphError::invalid_input(message));
    }
    Ok(vector.map(|value| value / length))
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ViewData {
    matrix: [[f32; 4]; 4],
    camera_words: [f32; 16],
    shadow: shadow::ShadowData,
}

impl ViewData {
    pub(super) fn matrix(&self) -> &[[f32; 4]; 4] {
        &self.matrix
    }

    pub(super) fn shadow_matrices(&self) -> &[[[f32; 4]; 4]; 4] {
        &self.shadow.matrices
    }

    pub(super) fn uniform_bytes(&self) -> [u8; CAMERA_BYTES] {
        let mut bytes = [0; CAMERA_BYTES];
        let words = self
            .matrix
            .as_flattened()
            .iter()
            .chain(&self.camera_words)
            .chain(self.shadow.words());
        for (out, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *out = value.to_le_bytes();
        }
        bytes
    }
}

pub(super) fn prepare(
    camera: Camera,
    sun: Sun,
    bounds: Bounds,
    extent: [u32; 2],
    settings: ShadowSettings,
    environment: [f32; 3],
) -> Result<ViewData, GraphError> {
    if environment.into_iter().any(|value| !value.is_finite())
        || environment[0] < 0.0
        || environment[2] < 0.0
    {
        return Err(GraphError::invalid_input(
            "invalid PBR environment parameters",
        ));
    }
    let aspect = extent[0] as f32 / extent[1] as f32;
    let matrix = camera.matrix(aspect)?;
    let shadow = shadow::calculate(camera, aspect, sun.direction, bounds, settings)?;
    // The eye and light occupy three vec4s, with near/far in their w words.
    // The fourth vec4 carries IBL sampling parameters and a shadow flag.
    let mut camera_words = [0.0; 16];
    camera_words[..3].copy_from_slice(&camera.position);
    camera_words[3] = camera.near;
    camera_words[4..7].copy_from_slice(&sun.direction);
    camera_words[7] = camera.far;
    camera_words[8..11].copy_from_slice(&sun.color);
    camera_words[12..15].copy_from_slice(&environment);
    camera_words[15] = if settings.enabled { 1.0 } else { 0.0 };
    Ok(ViewData {
        matrix,
        camera_words,
        shadow,
    })
}

/// Alpha interpretation of the bundled Forward and shadow shaders.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Mask,
    Blend,
}

/// Material constants encoded at asset creation or update. Pass them to
/// [`super::MaterialUniform::upload`] before using the material in a recipe.
/// The default is a white dielectric with unit normal and AO strengths.
#[derive(Clone, Copy, Debug)]
pub struct MaterialFactors {
    pub base_color: [f32; 4],
    pub emissive: [f32; 3],
    pub normal_scale: f32,
    pub metallic: f32,
    pub roughness: f32,
    pub occlusion_strength: f32,
    pub alpha_cutoff: f32,
    pub alpha_mode: AlphaMode,
    pub double_sided: bool,
}

impl Default for MaterialFactors {
    fn default() -> Self {
        Self {
            base_color: [1.0; 4],
            emissive: [0.0; 3],
            normal_scale: 1.0,
            metallic: 0.0,
            roughness: 1.0,
            occlusion_strength: 1.0,
            alpha_cutoff: 0.5,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
        }
    }
}

impl MaterialFactors {
    /// Encode the standard shader ABI for a custom Graph or cooked asset.
    pub fn encode(self) -> Result<[u8; MATERIAL_BYTES], GraphError> {
        if self
            .base_color
            .into_iter()
            .chain([self.metallic, self.roughness, self.occlusion_strength])
            .any(|value| !(0.0..=1.0).contains(&value))
            || self
                .emissive
                .into_iter()
                .any(|value| !value.is_finite() || value < 0.0)
            || !self.normal_scale.is_finite()
            || !self.alpha_cutoff.is_finite()
            || self.alpha_cutoff < 0.0
        {
            return Err(GraphError::invalid_input("invalid PBR material factors"));
        }
        let floats = self.base_color.into_iter().chain(self.emissive).chain([
            self.normal_scale,
            self.metallic,
            self.roughness,
            self.occlusion_strength,
            self.alpha_cutoff,
        ]);
        let mode: u32 = match self.alpha_mode {
            AlphaMode::Opaque => 0,
            AlphaMode::Mask => 1,
            AlphaMode::Blend => 2,
        };
        let mut bytes = [0; MATERIAL_BYTES];
        let words = floats
            .map(f32::to_le_bytes)
            .chain([mode, u32::from(self.double_sided), 0, 0].map(u32::to_le_bytes));
        for (out, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *out = value;
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera {
            position: [0.0, 0.0, 4.0],
            target: [0.0; 3],
            fov_y_degrees: 60.0,
            near: 0.02,
            far: 500.0,
        }
    }

    #[test]
    fn view_packs_projection_sun_ibl_and_each_cascade_in_shader_order() {
        let camera = camera();
        let sun = Sun::new([1.0, 2.0, 3.0], [0.5, 1.0, 4.0]).unwrap();
        let reused = Sun::from_normalized(sun.direction, sun.color).unwrap();
        assert_eq!(
            reused.direction.map(f32::to_bits),
            sun.direction.map(f32::to_bits)
        );
        let view = prepare(
            camera,
            reused,
            Bounds::new([-1.0; 3], [1.0; 3]).unwrap(),
            [128, 96],
            ShadowSettings::default(),
            [0.7, -0.2, 3.0],
        )
        .unwrap();
        let bytes = view.uniform_bytes();
        let words = bytes.as_chunks::<4>().0;
        assert!(
            words[..16].iter().copied().eq(view
                .matrix()
                .as_flattened()
                .iter()
                .copied()
                .map(f32::to_le_bytes))
        );
        assert_eq!(
            words[16..32],
            [
                0.0,
                0.0,
                4.0,
                0.02,
                sun.direction[0],
                sun.direction[1],
                sun.direction[2],
                500.0,
                0.5,
                1.0,
                4.0,
                0.0,
                0.7,
                -0.2,
                3.0,
                1.0
            ]
            .map(f32::to_le_bytes)
        );
        assert!(
            words[32..96].iter().copied().eq(view
                .shadow_matrices()
                .as_flattened()
                .as_flattened()
                .iter()
                .copied()
                .map(f32::to_le_bytes))
        );
        assert_eq!(f32::from_le_bytes(words[99]), 120.0);
    }

    #[test]
    fn material_packing_matches_float_and_flag_slots() {
        let values = MaterialFactors {
            base_color: [0.1, 0.2, 0.3, 0.4],
            emissive: [1.0, 2.0, 3.0],
            normal_scale: -1.0,
            metallic: 0.5,
            roughness: 0.6,
            occlusion_strength: 0.9,
            alpha_cutoff: 2.0,
            alpha_mode: AlphaMode::Mask,
            double_sided: true,
        };
        let bytes = values.encode().unwrap();
        let words = bytes.as_chunks::<4>().0;
        assert_eq!(
            words[..12],
            [
                0.1f32, 0.2, 0.3, 0.4, 1.0, 2.0, 3.0, -1.0, 0.5, 0.6, 0.9, 2.0
            ]
            .map(f32::to_le_bytes)
        );
        assert_eq!(words[12..], [1u32, 1, 0, 0].map(u32::to_le_bytes));
        assert!(
            MaterialFactors {
                roughness: f32::NAN,
                ..values
            }
            .encode()
            .is_err()
        );
    }

    #[test]
    fn rejects_invalid_input_before_upload() {
        let mut camera = camera();
        camera.target = camera.position;
        let sun = Sun::new([0.0, 1.0, 0.0], [1.0; 3]).unwrap();
        let bounds = Bounds::new([0.0; 3], [0.0; 3]).unwrap();
        assert!(
            prepare(
                camera,
                sun,
                bounds,
                [128, 96],
                ShadowSettings::default(),
                [0.0; 3]
            )
            .is_err()
        );
        assert!(Sun::from_normalized([0.0, 2.0, 0.0], [1.0; 3]).is_err());
        assert!(Bounds::new([1.0; 3], [0.0; 3]).is_err());
        assert!(
            ShadowSettings {
                resolution: 4,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}

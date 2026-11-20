//! Typed inputs and host packing for the reviewed Forward shader ABI.

mod input;
mod shadow;

use crate::passes::pbr::{CAMERA_BYTES, MATERIAL_BYTES};
use renderer::graph::GraphError;

pub use input::{Projection, ViewInput};
pub use shadow::{Bounds, ShadowSettings};

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
    shadow: Option<shadow::ShadowData>,
    unshadowed_forward: [f32; 3],
}

impl ViewData {
    pub(super) fn matrix(&self) -> &[[f32; 4]; 4] {
        &self.matrix
    }

    pub(super) fn shadow_matrices(&self) -> Option<&[[[f32; 4]; 4]; 4]> {
        self.shadow.as_ref().map(|shadow| &shadow.matrices)
    }

    pub(super) fn uniform_bytes(&self) -> [u8; CAMERA_BYTES] {
        let mut bytes = [0; CAMERA_BYTES];
        let words = self
            .matrix
            .as_flattened()
            .iter()
            .chain(&self.camera_words)
            .chain(self.shadow.iter().flat_map(shadow::ShadowData::words));
        for (out, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *out = value.to_le_bytes();
        }
        if self.shadow.is_none() {
            // Orthographic lighting needs a constant view direction even
            // without the cascades that otherwise supply these words.
            for (out, value) in bytes.as_chunks_mut::<4>().0[108..111]
                .iter_mut()
                .zip(self.unshadowed_forward)
            {
                *out = value.to_le_bytes();
            }
        }
        bytes
    }
}

pub(super) fn prepare(
    view: ViewInput,
    sun: Sun,
    bounds: Bounds,
    settings: Option<ShadowSettings>,
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
    let shadow = settings
        .map(|settings| shadow::calculate(view, sun.direction, bounds, settings))
        .transpose()?;
    // The eye and light occupy three vec4s, with near/far in their w words.
    // The fourth vec4 carries IBL sampling parameters and a shadow flag.
    let mut camera_words = [0.0; 16];
    camera_words[..3].copy_from_slice(&view.eye);
    camera_words[3] = view.near_far[0];
    camera_words[4..7].copy_from_slice(&sun.direction);
    camera_words[7] = view.near_far[1];
    camera_words[8..11].copy_from_slice(&sun.color);
    let orthographic = matches!(view.shape, input::Shape::Orthographic(_));
    camera_words[11] = if orthographic { 1.0 } else { 0.0 };
    camera_words[12..15].copy_from_slice(&environment);
    camera_words[15] = if shadow.is_some() { 1.0 } else { 0.0 };
    Ok(ViewData {
        matrix: view.matrix,
        camera_words,
        shadow,
        unshadowed_forward: if orthographic {
            view.forward.map(|coordinate| coordinate as f32)
        } else {
            [0.0; 3]
        },
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

/// Material constants. The caller uploads these bytes at asset creation or
/// update, then supplies that buffer and its four sampled textures per frame.
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
            normal_scale: 0.0,
            metallic: 0.0,
            roughness: 1.0,
            occlusion_strength: 0.0,
            alpha_cutoff: 0.5,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
        }
    }
}

impl MaterialFactors {
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
fn test_perspective([near, far]: [f32; 2]) -> ViewInput {
    let aspect = 4.0 / 3.0;
    let scale = 1.0 / (60.0_f32.to_radians() * 0.5).tan();
    let z_scale = far / (near - far);
    let z_offset = near * far / (near - far);
    ViewInput::new(
        [
            [scale / aspect, 0.0, 0.0, 0.0],
            [0.0, -scale, 0.0, 0.0],
            [0.0, 0.0, z_scale, -1.0],
            [0.0, 0.0, z_scale * -4.0 + z_offset, 4.0],
        ],
        [0.0, 0.0, 4.0],
        [0.0, 0.0, -1.0],
        [near, far],
        Projection::SymmetricPerspective {
            vertical_fov_degrees: 60.0,
            aspect,
        },
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_packs_projection_sun_ibl_and_each_cascade_in_shader_order() {
        let input = test_perspective([0.02, 500.0]);
        let sun = Sun::new([1.0, 2.0, 3.0], [0.5, 1.0, 4.0]).unwrap();
        let reused = Sun::from_normalized(sun.direction, sun.color).unwrap();
        assert_eq!(
            reused.direction.map(f32::to_bits),
            sun.direction.map(f32::to_bits)
        );
        let view = prepare(
            input,
            reused,
            Bounds::new([-1.0; 3], [1.0; 3]).unwrap(),
            Some(ShadowSettings::default()),
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
                .unwrap()
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
    fn disabled_shadows_do_not_require_cascade_splits() {
        let input = test_perspective([200.0, 500.0]);
        let sun = Sun::new([0.0, 1.0, 0.0], [1.0; 3]).unwrap();
        let bounds = Bounds::new([0.0; 3], [0.0; 3]).unwrap();
        let view = prepare(input, sun, bounds, None, [0.0; 3]).unwrap();
        assert!(view.shadow_matrices().is_none());
        // The disabled shadow flag and all unused cascade fields are zero.
        assert!(view.uniform_bytes()[124..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn rejects_invalid_input_before_upload() {
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

    #[test]
    fn orthographic_overhead_view_retains_direction_and_covers_shadow_receivers() {
        let input = ViewInput::new(
            [
                [0.5, 0.0, 0.0, 0.0],
                [0.0, 0.0, -0.25, 0.0],
                [0.0, 0.5, 0.0, 0.0],
                [0.0, 0.0, 0.75, 1.0],
            ],
            [0.0, 4.0, 0.0],
            [0.0, -1.0, 0.0],
            [1.0, 5.0],
            Projection::Orthographic,
        )
        .unwrap();
        let sun = Sun::new([0.0, 1.0, 0.0], [1.0; 3]).unwrap();
        let bounds = Bounds::new([-2.0, -1.0, -2.0], [2.0, 3.0, 2.0]).unwrap();
        let data = prepare(
            input,
            sun,
            bounds,
            Some(ShadowSettings::default()),
            [0.0; 3],
        )
        .unwrap();
        let bytes = data.uniform_bytes();
        let words = bytes.as_chunks::<4>().0;
        assert_eq!(f32::from_le_bytes(words[27]), 1.0);
        assert_eq!(words[108..111], [0.0_f32, -1.0, 0.0].map(f32::to_le_bytes));
        let disabled = prepare(input, sun, bounds, None, [0.0; 3]).unwrap();
        assert_eq!(disabled.uniform_bytes()[432..444], bytes[432..444]);
        let first_cascade = data.shadow_matrices().unwrap()[0];
        for [x, z] in [[-2.0, -2.0], [-2.0, 2.0], [2.0, -2.0], [2.0, 2.0]] {
            assert!((first_cascade[0][0] * x + first_cascade[3][0]).abs() < 1.0);
            assert!((first_cascade[2][1] * z + first_cascade[3][1]).abs() < 1.0);
        }
    }
}

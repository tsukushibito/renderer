//! World-space emitters shared with the offline lighting baker.
use renderer_core::graph::GraphError;
use std::f32::consts::PI;

/// A finite, uniformly emitting, one-sided Lambertian rectangle in metres.
/// The cross product of the two half-edge vectors points toward the lit side.
#[derive(Clone, Copy, Debug)]
pub struct RectLight {
    center: [f32; 3],
    half_width: [f32; 3],
    half_height: [f32; 3],
    radiance: [f32; 3],
    range: f32,
    area: f32,
    pub(crate) bound_radius: f32,
}

impl RectLight {
    /// Total white luminous flux is spread over the emitting face. Linear RGB
    /// tints its radiance. Range is the cutoff distance from the nearest point
    /// on the rectangle, independent of its physical size.
    pub fn new(
        center: [f32; 3],
        half_width: [f32; 3],
        half_height: [f32; 3],
        color: [f32; 3],
        lumens: f32,
        range: f32,
    ) -> Result<Self, GraphError> {
        let dot = |a: [f32; 3], b: [f32; 3]| a.into_iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let u = dot(half_width, half_width).sqrt();
        let v = dot(half_height, half_height).sqrt();
        let area = 4. * u * v;
        let radiance = color.map(|c| c * (lumens / area / PI));
        let bound_radius = range + u.hypot(v);
        if !center.into_iter().all(f32::is_finite)
            || !area.is_normal()
            || !u.is_normal()
            || !v.is_normal()
            || dot(half_width, half_height).abs() > 1e-4 * u * v
            || !range.is_normal()
            || range <= 0.
            || !bound_radius.is_finite()
            || !(range * range).is_normal()
            || !lumens.is_finite()
            || lumens < 0.
            || color
                .iter()
                .chain(&radiance)
                .any(|&v| !v.is_finite() || v < 0.)
        {
            return Err(GraphError::invalid_input("invalid rectangular area light"));
        }
        Ok(Self {
            center,
            half_width,
            half_height,
            range,
            radiance,
            area,
            bound_radius,
        })
    }

    pub fn center(self) -> [f32; 3] {
        self.center
    }
    pub fn half_edges(self) -> [[f32; 3]; 2] {
        [self.half_width, self.half_height]
    }
    pub fn radiance(self) -> [f32; 3] {
        self.radiance
    }
    pub fn range(self) -> f32 {
        self.range
    }
    pub fn area(self) -> f32 {
        self.area
    }
    pub fn normal(self) -> [f32; 3] {
        let [a, b] = self.half_edges();
        std::array::from_fn(|i| {
            let j = (i + 1) % 3;
            let k = (i + 2) % 3;
            (a[j] * b[k] - a[k] * b[j]) / (self.area * 0.25)
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum LocalLight {
    /// Linear RGB and intensity, with a smooth cutoff at radius metres.
    Point {
        position: [f32; 3],
        radius: f32,
        color: [f32; 3],
        intensity: f32,
    },
    Rect(RectLight),
    /// Evaluate direct visibility from the emission centre with a shadow map.
    ShadowedRect(RectLight),
}

impl LocalLight {
    pub fn shadowed_rect(light: RectLight) -> Self {
        Self::ShadowedRect(light)
    }

    pub(crate) fn shadow_emitter(&self) -> Option<RectLight> {
        match *self {
            Self::ShadowedRect(rect) => Some(rect),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flux_is_conserved_across_sizes_and_winding_sets_the_emitting_side() {
        for width in [0.25, 1., 5.] {
            let light = RectLight::new(
                [0.; 3],
                [width, 0., 0.],
                [0., 0., 0.5],
                [1., 0.5, 0.],
                300.,
                10.,
            )
            .unwrap();
            assert!((light.radiance()[0] * light.area() * PI - 300.).abs() < 1e-4);
            assert_eq!(light.normal(), [0., -1., 0.]);
        }
        for edge in [[0.; 3], [1., 1., 0.], [f32::NAN; 3]] {
            assert!(RectLight::new([0.; 3], [1., 0., 0.], edge, [1.; 3], 1., 10.).is_err());
        }
    }
}

//! World-space emitters shared with the offline lighting baker.
use renderer_core::graph::GraphError;

/// Scale a nonnegative linear Rec.709 tint so its luminance (Y) equals the
/// requested photometric magnitude. Use the same convention for lux, cd and nits.
pub fn photometric_rgb(tint: [f32; 3], magnitude: f32) -> Result<[f32; 3], GraphError> {
    scaled_rgb(tint, f64::from(magnitude))
}

fn scaled_rgb(tint: [f32; 3], magnitude: f64) -> Result<[f32; 3], GraphError> {
    if !magnitude.is_finite() || magnitude < 0. || tint.iter().any(|&v| !v.is_finite() || v < 0.) {
        return Err(GraphError::invalid_input(
            "photometric color and magnitude must be finite and nonnegative",
        ));
    }
    if magnitude == 0. {
        return Ok([0.; 3]);
    }
    let [r, g, b] = tint.map(f64::from);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if y == 0. {
        return Err(GraphError::invalid_input(
            "positive light needs a nonblack tint",
        ));
    }
    let rgb = tint.map(|v| (f64::from(v) * magnitude / y) as f32);
    if !rgb.iter().all(|v| v.is_finite()) {
        return Err(GraphError::invalid_input(
            "photometric RGB exceeds float range",
        ));
    }
    Ok(rgb)
}

/// Isotropic emitter in metres. Linear RGB candela already includes the tint.
#[derive(Clone, Copy, Debug)]
pub struct PointLight {
    position: [f32; 3],
    range: f32,
    rgb_candela: [f32; 3],
}
impl PointLight {
    pub fn from_candela(
        position: [f32; 3],
        range: f32,
        tint: [f32; 3],
        candela: f32,
    ) -> Result<Self, GraphError> {
        Self::at(position, range, photometric_rgb(tint, candela)?)
    }

    /// Total luminous flux emitted into 4π steradians, after tinting.
    pub fn from_lumens(
        position: [f32; 3],
        range: f32,
        tint: [f32; 3],
        lumens: f32,
    ) -> Result<Self, GraphError> {
        let candela = f64::from(lumens) / (4. * std::f64::consts::PI);
        Self::at(position, range, scaled_rgb(tint, candela)?)
    }

    fn at(position: [f32; 3], range: f32, rgb_candela: [f32; 3]) -> Result<Self, GraphError> {
        if !position.iter().all(|v| v.is_finite()) || range <= 0. || !(range * range).is_normal() {
            return Err(GraphError::invalid_input(
                "invalid point light position or range in metres",
            ));
        }
        Ok(Self {
            position,
            range,
            rgb_candela,
        })
    }
    pub fn position(self) -> [f32; 3] {
        self.position
    }
    pub fn range(self) -> f32 {
        self.range
    }
    pub fn rgb_candela(self) -> [f32; 3] {
        self.rgb_candela
    }
}

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
    /// Total luminous flux after tinting, spread over the emitting face.
    /// Range is the cutoff distance in metres from the nearest point on the rectangle.
    pub fn from_lumens(
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
        {
            return Err(GraphError::invalid_input("invalid rectangular area light"));
        }
        let radiance = scaled_rgb(
            color,
            f64::from(lumens) / (f64::from(area) * std::f64::consts::PI),
        )?;
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
    pub fn rgb_nits(self) -> [f32; 3] {
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
    Point(PointLight),
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
    use std::f32::consts::PI;

    #[test]
    fn tint_preserves_photometric_magnitude() {
        for tint in [[1.; 3], [0., 0., 0.2], [0.1, 2., 3.]] {
            let rgb = photometric_rgb(tint, 100.).unwrap();
            let y = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
            assert!((y - 100.).abs() < 1e-4);
            let point = PointLight::from_lumens([0.; 3], 10., tint, 4. * PI * 100.).unwrap();
            for (actual, expected) in point.rgb_candela().into_iter().zip(rgb) {
                assert!((actual - expected).abs() <= expected * 2. * f32::EPSILON);
            }
        }
        assert!(photometric_rgb([0.; 3], 1.).is_err());
        assert_eq!(photometric_rgb([0.; 3], 0.).unwrap(), [0.; 3]);
        assert!(photometric_rgb([-1.; 3], 0.).is_err());
    }

    #[test]
    fn flux_is_conserved_across_sizes_and_winding_sets_the_emitting_side() {
        for width in [0.25, 1., 5.] {
            let light = RectLight::from_lumens(
                [0.; 3],
                [width, 0., 0.],
                [0., 0., 0.5],
                [1., 0.5, 0.],
                300.,
                10.,
            )
            .unwrap();
            let [r, g, b] = light.rgb_nits();
            assert!(
                ((0.2126 * r + 0.7152 * g + 0.0722 * b) * light.area() * PI - 300.).abs() < 1e-4
            );
            assert_eq!(light.normal(), [0., -1., 0.]);
        }
        for edge in [[0.; 3], [1., 1., 0.], [f32::NAN; 3]] {
            assert!(RectLight::from_lumens([0.; 3], [1., 0., 0.], edge, [1.; 3], 1., 10.).is_err());
        }
    }
}

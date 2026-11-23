//! Offline directional integration of the indirect specular term at a shaded receiver.
use crate::{BakeError, BakeScene, Lighting, Vec3, cross, dot, finite, transport::offset, unit};
use std::f32::consts::TAU;

pub struct ReflectionReceiver {
    pub position: Vec3,
    /// Face normal for the ray-origin offset.
    pub geometric_normal: Vec3,
    /// Includes the receiver's normal map.
    pub shading_normal: Vec3,
    /// Direction towards the camera.
    pub view: Vec3,
    pub f0: Vec3,
    pub roughness: f32,
}

#[derive(Default)]
pub struct ReflectionReference {
    /// Contributions to outgoing specular radiance split by first-ray hit/miss.
    pub from_geometry: Vec3,
    pub from_sky: Vec3,
    /// Identical rays and BRDF with all geometry removed.
    pub unobstructed: Vec3,
    /// Counts under the GGX sampling distribution, among above-horizon rays.
    pub sky_rays: u32,
    pub surface_rays: u32,
}

impl BakeScene {
    /// Integrate GGX/Smith/Schlick using the same static incident transport as local probes.
    /// Excludes primary direct sunlight; sun on secondary hits is included.
    pub fn reflection_reference(
        &self,
        s: &ReflectionReceiver,
        light: Lighting,
        samples: u32,
    ) -> Result<ReflectionReference, BakeError> {
        light.validate(self)?;
        let n = unit(s.shading_normal)?;
        let v = unit(s.view)?;
        let nv = dot(n, v);
        if samples == 0
            || !finite(s.position)
            || nv <= 0.
            || !s
                .f0
                .iter()
                .chain([&s.roughness])
                .all(|x| (0.0..=1.0).contains(x))
        {
            return Err(BakeError("invalid reflection receiver or sample count"));
        }
        let origin = offset(s.position, unit(s.geometric_normal)?);
        let axis = if n[1].abs() < 0.99 {
            [0., 1., 0.]
        } else {
            [1., 0., 0.]
        };
        let tangent = unit(cross(axis, n))?;
        let bitangent = cross(n, tangent);
        let a2 = s.roughness.powi(2).max(0.001).powi(2);
        let smith = |x: f32| (a2 + (1. - a2) * x * x).sqrt();
        let mut geometry = [0f64; 3];
        let mut sky = [0f64; 3];
        let mut open = [0f64; 3];
        let mut result = ReflectionReference::default();
        for j in 0..samples {
            // Sample GGX half-vectors with density D(h) * N.H.
            let u = (f64::from(j) + 0.5) / f64::from(samples);
            let nh = ((1. - u) / (1. - u + u * f64::from(a2))).sqrt() as f32;
            let radial = (1. - nh * nh).max(0.).sqrt();
            let (sin, cos) = (TAU * (j.reverse_bits() as f32 / 4294967296.)).sin_cos();
            let h = std::array::from_fn(|k| {
                nh * n[k] + radial * (cos * tangent[k] + sin * bitangent[k])
            });
            let vh = dot(v, h);
            let ray = std::array::from_fn(|k| 2. * vh * h[k] - v[k]);
            let nl = dot(n, ray);
            if vh <= 0. || nl <= 0. {
                continue;
            }
            let mut state = j;
            let (radiance, hit) = self.incoming_classified(origin, ray, light, &mut state);
            let (acc, env) = if hit {
                result.surface_rays += 1;
                (&mut geometry, self.sky_radiance(light, ray))
            } else {
                result.sky_rays += 1;
                (&mut sky, radiance)
            };
            // f_spec * N.L / pdf(L), where pdf(L)=D(h)*N.H/(4*V.H).
            let scale = 2. * nl * vh / (nh * (nv * smith(nl) + nl * smith(nv)).max(1e-6));
            let schlick = (1. - vh.clamp(0., 1.)).powi(5);
            for k in 0..3 {
                let weight = f64::from(scale * (s.f0[k] + (1. - s.f0[k]) * schlick));
                acc[k] += weight * f64::from(radiance[k]);
                open[k] += weight * f64::from(env[k]);
            }
        }
        let mean = |rgb: [f64; 3]| rgb.map(|x| (x / f64::from(samples)) as f32);
        result.from_geometry = mean(geometry);
        result.from_sky = mean(sky);
        result.unobstructed = mean(open);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Material, Triangle};
    #[test]
    fn white_furnace_matches_normal_incidence_ggx_integral() {
        let scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        let receiver = ReflectionReceiver {
            position: [0.; 3],
            geometric_normal: [0., 1., 0.],
            shading_normal: [0., 1., 0.],
            view: [0., 1., 0.],
            f0: [1.; 3],
            roughness: 1.,
        };
        let result = scene
            .reflection_reference(&receiver, Lighting::uniform_sky([1.; 3]), 4096)
            .unwrap();
        // At alpha=1, V=N, F=1, the integral reduces to integral_0^1 x/(1+x) dx.
        assert!((result.from_sky[0] - (1. - std::f32::consts::LN_2)).abs() < 1e-5);
    }
    #[test]
    fn opaque_black_plane_replaces_sky_only_on_intersecting_rays() {
        let tri = Triangle {
            positions: [[-10., 2., -10.], [10., 2., -10.], [0., 2., 10.]],
            uv: [[0.; 2]; 3],
            material: 0,
        };
        let scene = BakeScene::new(
            vec![tri],
            vec![Material {
                diffuse: [0.; 3],
                ..Default::default()
            }],
            vec![],
        )
        .unwrap();
        let empty = BakeScene::new(vec![], vec![], vec![]).unwrap();
        let s = ReflectionReceiver {
            position: [0.; 3],
            geometric_normal: [0., 1., 0.],
            shading_normal: [0., 1., 0.],
            view: [0., 1., 0.],
            f0: [0.04; 3],
            roughness: 0.6,
        };
        let light = Lighting::uniform_sky([1.; 3]);
        let a = empty.reflection_reference(&s, light, 2048).unwrap();
        let b = scene.reflection_reference(&s, light, 2048).unwrap();
        assert_eq!(a.from_sky, a.unobstructed);
        assert_eq!(a.unobstructed, b.unobstructed);
        assert!(b.surface_rays > 0 && b.sky_rays > 0);
        assert_eq!(b.from_geometry, [0.; 3]);
        assert!(b.from_sky[0] < a.from_sky[0]);
    }
}

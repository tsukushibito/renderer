use crate::{BakeError, BakeScene, Vec3, cross, dot, finite, nonnegative, unit};
use std::f32::consts::{PI, TAU};

/// Sky radiance for escaping rays and normal-incidence solar irradiance.
#[derive(Clone, Copy)]
pub struct Lighting {
    /// Linear Rec.709 sky color in cd/m² (Y), for a unit white texel when textured.
    pub sky_rgb_nits: Vec3,
    pub environment: Option<SkyMap>,
    pub sun: crate::Sun,
}
/// Top-origin linear HDR latitude-longitude texture in the bake scene.
#[derive(Clone, Copy)]
pub struct SkyMap {
    pub texture: usize,
    pub yaw_radians: f32,
}

/// Supply one lighting condition directly, or an array of equally spaced lighting keyframes.
pub trait LightingScenarios<const N: usize> {
    fn scenarios(self) -> [Lighting; N];
}
impl LightingScenarios<1> for Lighting {
    fn scenarios(self) -> [Lighting; 1] {
        [self]
    }
}
impl<const N: usize> LightingScenarios<N> for [Lighting; N] {
    fn scenarios(self) -> [Lighting; N] {
        self
    }
}
#[derive(Clone, Copy)]
pub struct SurfaceSample {
    pub position: Vec3,
    pub normal: Vec3,
}

pub(crate) fn offset(p: Vec3, n: Vec3) -> Vec3 {
    let epsilon = p.iter().fold(1f32, |scale, x| scale.max(x.abs())) * 4. * f32::EPSILON;
    std::array::from_fn(|i| p[i] + epsilon * n[i])
}
fn hemisphere(n: Vec3, u: f32, v: f32) -> Vec3 {
    let axis = if n[2].abs() < 0.9 {
        [0., 0., 1.]
    } else {
        [0., 1., 0.]
    };
    let t = unit(cross(axis, n)).unwrap();
    let b = cross(n, t);
    let (sin, cos) = (TAU * v).sin_cos();
    let r = u.sqrt();
    std::array::from_fn(|i| r * (cos * t[i] + sin * b[i]) + (1. - u).sqrt() * n[i])
}
fn random(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(747796405).wrapping_add(2891336453);
    let bits = ((*state >> ((*state >> 28) + 4)) ^ *state).wrapping_mul(277803737);
    ((bits ^ (bits >> 22)) >> 8) as f32 / 16777216.
}
impl Lighting {
    /// Illuminate the scene with a uniform linear HDR sky.
    pub fn uniform_sky(rgb_nits: Vec3) -> Self {
        Self {
            sky_rgb_nits: rgb_nits,
            environment: None,
            sun: crate::Sun::OFF,
        }
    }
    /// Use a lat-long environment shared with the runtime. A unit white texel emits the specified luminance in cd/m².
    pub fn environment_map(map: SkyMap, nits_per_unit: f32) -> Self {
        Self {
            environment: Some(map),
            ..Self::uniform_sky([nits_per_unit; 3])
        }
    }
    #[must_use]
    pub fn with_sun(mut self, sun: crate::Sun) -> Self {
        self.sun = sun;
        self
    }
    pub(crate) fn validate(&self, scene: &BakeScene) -> Result<(), BakeError> {
        if !nonnegative(self.sky_rgb_nits) {
            return Err(BakeError("invalid lighting energy"));
        }
        if let Some(map) = self.environment
            && (!map.yaw_radians.is_finite() || map.texture >= scene.textures.len())
        {
            return Err(BakeError("invalid sky texture or yaw"));
        }
        Ok(())
    }
}
impl BakeScene {
    pub(crate) fn lighting<const N: usize>(
        &self,
        scenarios: impl LightingScenarios<N>,
    ) -> Result<[Lighting; N], BakeError> {
        if !(1..=renderer::pbr::BakedLighting::MAX_SCENARIOS).contains(&N) {
            return Err(BakeError("unsupported lighting scenario count"));
        }
        let lights = scenarios.scenarios();
        for light in &lights {
            light.validate(self)?;
        }
        Ok(lights)
    }

    pub(crate) fn sky_radiance(&self, light: Lighting, ray: Vec3) -> Vec3 {
        let Some(map) = light.environment else {
            return light.sky_rgb_nits;
        };
        let texture = &self.textures[map.texture];
        let u = 0.5 + (ray[2].atan2(ray[0]) + map.yaw_radians) / TAU;
        let half = 0.5 / texture.height as f32;
        let v = (ray[1].clamp(-1., 1.).acos() / PI).clamp(half, 1. - half);
        let rgba = texture.sample([u, v], crate::SamplerDescriptor::default());
        std::array::from_fn(|i| light.sky_rgb_nits[i] * rgba[i])
    }

    /// Isotropic fallback beyond the baked volume, weighted by texel solid angle.
    pub(crate) fn mean_sky(&self, light: Lighting) -> Vec3 {
        let Some(map) = light.environment else {
            return light.sky_rgb_nits;
        };
        let t = &self.textures[map.texture];
        let mut sum = [0f64; 3];
        for (y, row) in t.pixels.chunks_exact(t.width).enumerate() {
            let step = std::f64::consts::PI / t.height as f64;
            let weight =
                ((y as f64 * step).cos() - ((y + 1) as f64 * step).cos()) / (2. * t.width as f64);
            for rgba in row {
                for i in 0..3 {
                    sum[i] += f64::from(rgba[i]) * weight;
                }
            }
        }
        std::array::from_fn(|i| sum[i] as f32 * light.sky_rgb_nits[i])
    }

    pub(crate) fn incoming(
        &self,
        origin: Vec3,
        ray: Vec3,
        light: Lighting,
        state: &mut u32,
    ) -> Vec3 {
        self.incoming_classified(origin, ray, light, state).0
    }

    // Point lighting and uniform area sampling with visibility rays are evaluated
    // at secondary surface hits only, including those in reflection captures.
    fn local_illuminance(&self, origin: Vec3, normal: Vec3, mut seed: u32) -> Vec3 {
        let mut sum = [0.; 3];
        for point in &self.point_lights {
            let delta = crate::sub(point.position(), origin);
            let d2 = dot(delta, delta);
            let radial = d2 / point.range().powi(2);
            if radial >= 1. {
                continue;
            }
            let distance = d2.sqrt();
            let direction = if d2 > 1e-20 {
                delta.map(|v| v / distance)
            } else {
                normal
            };
            let cosine = dot(normal, direction).max(0.);
            if cosine > 0. && self.trace(origin, direction, distance).is_none() {
                let scale = cosine * (1. - radial * radial).powi(2) / d2.max(0.01);
                for (channel, intensity) in sum.iter_mut().zip(point.rgb_candela()) {
                    *channel += intensity * scale;
                }
            }
        }
        for rect in &self.rect_lights {
            let [u, v] = rect.half_edges();
            let center = crate::sub(rect.center(), origin);
            let coordinate = |edge| (dot(center, edge) / dot(edge, edge)).clamp(-1., 1.);
            let (x, y) = (coordinate(u), coordinate(v));
            let nearest = std::array::from_fn(|i| center[i] - x * u[i] - y * v[i]);
            let radial = dot(nearest, nearest) / rect.range().powi(2);
            if radial >= 1. {
                continue;
            }
            let (x, y) = (2. * random(&mut seed) - 1., 2. * random(&mut seed) - 1.);
            let delta = std::array::from_fn(|i| center[i] + x * u[i] + y * v[i]);
            let d2 = dot(delta, delta);
            if d2 <= 1e-12 {
                continue;
            }
            let dist = d2.sqrt();
            let l = delta.map(|c| c / dist);
            let cosine = dot(normal, l).max(0.) * (-dot(rect.normal(), l)).max(0.);
            if cosine > 0. && self.trace(origin, l, dist).is_none() {
                let scale = rect.area() * cosine / d2 * (1. - radial * radial).powi(2);
                for (channel, radiance) in sum.iter_mut().zip(rect.rgb_nits()) {
                    *channel += radiance * scale;
                }
            }
        }
        sum
    }

    /// Incident radiance and whether the initial ray hit static geometry.
    pub(crate) fn incoming_classified(
        &self,
        mut origin: Vec3,
        mut ray: Vec3,
        light: Lighting,
        state: &mut u32,
    ) -> (Vec3, bool) {
        let mut result = [0.; 3];
        let mut weight = [1.; 3];
        // Two diffuse surface reflections, with next-event estimation for sunlight.
        for bounce in 0..2 {
            let Some(hit) = self.trace(origin, ray, f32::INFINITY) else {
                let sky = self.sky_radiance(light, ray);
                for i in 0..3 {
                    result[i] += weight[i] * sky[i];
                }
                return (result, bounce != 0);
            };
            origin = offset(
                std::array::from_fn(|i| origin[i] + ray[i] * hit.distance),
                hit.normal,
            );
            let mut cosine = dot(hit.normal, light.sun.direction()).max(0.);
            if cosine > 0.
                && self
                    .trace(origin, light.sun.direction(), f32::INFINITY)
                    .is_some()
            {
                cosine = 0.;
            }
            let local = self.local_illuminance(origin, hit.normal, *state ^ 0x9e3779b9);
            for i in 0..3 {
                result[i] += weight[i]
                    * (hit.emission[i]
                        + hit.diffuse[i] * (light.sun.rgb_lux()[i] * cosine + local[i]) / PI);
                weight[i] *= hit.diffuse[i];
            }
            ray = hemisphere(hit.normal, random(state), random(state));
        }
        if self.trace(origin, ray, f32::INFINITY).is_none() {
            let sky = self.sky_radiance(light, ray);
            for i in 0..3 {
                result[i] += weight[i] * sky[i];
            }
        }
        (result, true)
    }

    /// Integrate diffuse irradiance at surface samples. The receiver material
    /// and direct illumination are applied later by the renderer.
    pub fn bake_irradiance(
        &self,
        surfaces: &[SurfaceSample],
        light: Lighting,
        samples: u32,
    ) -> Result<Vec<Vec3>, BakeError> {
        light.validate(self)?;
        if samples == 0 {
            return Err(BakeError("zero transport samples"));
        }
        if surfaces.is_empty() {
            return Ok(Vec::new());
        }
        let integrate = |s: &SurfaceSample| -> Result<Vec3, BakeError> {
            if !finite(s.position) {
                return Err(BakeError("invalid receiver position"));
            }
            let normal = unit(s.normal)?;
            let origin = offset(s.position, normal);
            let mut sum = [0f64; 3];
            for j in 0..samples {
                let mut state = j;
                let ray = hemisphere(
                    normal,
                    (j as f32 + 0.5) / samples as f32,
                    j.reverse_bits() as f32 / 4294967296.,
                );
                let value = self.incoming(origin, ray, light, &mut state);
                for i in 0..3 {
                    sum[i] += f64::from(value[i]);
                }
            }
            Ok(sum.map(|v| (v * f64::from(PI) / f64::from(samples)) as f32))
        };
        let mut result = vec![[0.; 3]; surfaces.len()];
        let workers = std::thread::available_parallelism().map_or(1, usize::from);
        let chunk = surfaces.len().div_ceil(workers).max(32);
        std::thread::scope(|scope| -> Result<(), BakeError> {
            let handles = surfaces
                .chunks(chunk)
                .zip(result.chunks_mut(chunk))
                .map(|(input, out)| {
                    let integrate = &integrate;
                    scope.spawn(move || -> Result<(), BakeError> {
                        for (s, v) in input.iter().zip(out) {
                            *v = integrate(s)?;
                        }
                        Ok(())
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                handle.join().expect("surface worker panicked")?;
            }
            Ok(())
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Material, Triangle};
    fn surface() -> SurfaceSample {
        SurfaceSample {
            position: [0.; 3],
            normal: [0., 1., 0.],
        }
    }
    #[test]
    fn empty_scene_integrates_sky_and_excludes_direct_sun() {
        let scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        let l = Lighting {
            environment: None,
            sky_rgb_nits: [0.5, 0.25, 1.],
            sun: renderer::pbr::Sun::from_lux([0., 1., 0.], [1.; 3], 10.).unwrap(),
        };
        let actual = scene.bake_irradiance(&[surface()], l, 16).unwrap()[0];
        for (v, sky) in actual.into_iter().zip(l.sky_rgb_nits) {
            assert!((v - PI * sky).abs() < 1e-5);
        }
    }
    #[test]
    fn calibrated_environment_gives_pi_times_luminance_on_a_white_receiver() {
        let mut scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        let texture = crate::Texture::new(2, 1, vec![[0.5, 0.5, 0.5, 1.]; 2]).unwrap();
        let map = scene.add_sky_map(texture, 0.).unwrap();
        let light = Lighting::environment_map(map, 40.);
        for e in scene.bake_irradiance(&[surface()], light, 32).unwrap()[0] {
            assert!((e - PI * 20.).abs() < 1e-5);
        }
    }

    #[test]
    fn point_light_illuminance_follows_inverse_square_in_metres() {
        let mut scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        scene.set_point_lights(vec![
            crate::PointLight::from_candela([0.; 3], 10000., [1.; 3], 100.).unwrap(),
        ]);
        for (distance, lux) in [(1., 100.), (2., 25.), (10000., 0.)] {
            let e = scene.local_illuminance([0., -distance, 0.], [0., 1., 0.], 0);
            assert_eq!(e, [lux; 3]);
        }
    }
    #[test]
    fn rectangle_illuminates_visible_bounce_surfaces_only() {
        let rect = crate::RectLight::from_lumens(
            [0., 2., 0.],
            [0.5, 0., 0.],
            [0., 0., 0.25],
            [1., 0., 0.],
            20.,
            8.,
        )
        .unwrap();
        let mut scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        scene.set_rect_lights(vec![rect]);
        let dark = Lighting::uniform_sky([0.; 3]);
        assert_eq!(
            scene.bake_irradiance(&[surface()], dark, 32).unwrap()[0],
            [0.; 3]
        );
        assert!(scene.local_illuminance([0.; 3], surface().normal, 1)[0] > 0.);
        let plane = |y| Triangle {
            positions: [[-10., y, -10.], [10., y, -10.], [0., y, 10.]],
            uv: [[0.; 2]; 3],
            material: 0,
        };
        let mut scene = BakeScene::new(vec![plane(0.)], vec![Material::default()], vec![]).unwrap();
        scene.set_rect_lights(vec![rect]);
        let bounce = scene
            .incoming_classified([0., 1., 0.], [0., -1., 0.], dark, &mut 1)
            .0;
        assert!(bounce[0] > 0.);
        assert_eq!(bounce[1..], [0.; 2]);
        let mut blocked =
            BakeScene::new(vec![plane(1.)], vec![Material::default()], vec![]).unwrap();
        blocked.set_rect_lights(vec![rect]);
        assert_eq!(
            blocked.local_illuminance([0.; 3], surface().normal, 1),
            [0.; 3]
        );
    }

    #[test]
    fn masked_ceiling_transmits_sky_while_opaque_ceiling_emits_red() {
        let tri = Triangle {
            positions: [[-100., 1., -100.], [100., 1., -100.], [0., 1., 100.]],
            uv: [[0.; 2]; 3],
            material: 0,
        };
        let material = Material {
            diffuse: [0.; 3],
            emissive_nits: [1., 0., 0.],
            base_texture: Some(0),
            alpha_cutoff: Some(0.5),
            ..Default::default()
        };
        let l = Lighting {
            environment: None,
            sky_rgb_nits: [0., 0., 1.],
            sun: crate::Sun::OFF,
        };
        for (alpha, expected) in [(0., [0., 0., PI]), (1., [PI, 0., 0.])] {
            let texture = crate::Texture::new(1, 1, vec![[1., 1., 1., alpha]]).unwrap();
            let scene = BakeScene::new(vec![tri], vec![material], vec![texture]).unwrap();
            let actual = scene.bake_irradiance(&[surface()], l, 32).unwrap()[0];
            for i in 0..3 {
                assert!((actual[i] - expected[i]).abs() < 1e-5);
            }
        }
    }
}

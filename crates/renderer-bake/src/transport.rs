use crate::{BakeError, BakeScene, Vec3, cross, dot, finite, nonnegative, unit};
use std::f32::consts::{PI, TAU};

/// Uniform sky radiance for escaping rays and normal-incidence solar irradiance.
#[derive(Clone, Copy)]
pub struct Lighting {
    pub sky: Vec3,
    pub direction_to_sun: Vec3,
    pub sun: Vec3,
}
#[derive(Clone, Copy)]
pub struct SurfaceSample {
    pub position: Vec3,
    pub normal: Vec3,
}

fn offset(p: Vec3, n: Vec3) -> Vec3 {
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
    pub(crate) fn normalized(self) -> Result<Self, BakeError> {
        if !nonnegative(self.sky) || !nonnegative(self.sun) {
            return Err(BakeError("invalid lighting energy"));
        }
        Ok(Self {
            direction_to_sun: unit(self.direction_to_sun)?,
            ..self
        })
    }
}
impl BakeScene {
    pub(crate) fn incoming(
        &self,
        mut origin: Vec3,
        mut ray: Vec3,
        light: Lighting,
        state: &mut u32,
    ) -> Vec3 {
        let mut result = [0.; 3];
        let mut weight = [1.; 3];
        // Two diffuse surface reflections, with next-event estimation for sunlight.
        for _ in 0..2 {
            let Some(hit) = self.trace(origin, ray, f32::INFINITY) else {
                for i in 0..3 {
                    result[i] += weight[i] * light.sky[i];
                }
                return result;
            };
            origin = offset(
                std::array::from_fn(|i| origin[i] + ray[i] * hit.distance),
                hit.normal,
            );
            let mut cosine = dot(hit.normal, light.direction_to_sun).max(0.);
            if cosine > 0.
                && self
                    .trace(origin, light.direction_to_sun, f32::INFINITY)
                    .is_some()
            {
                cosine = 0.;
            }
            for i in 0..3 {
                result[i] +=
                    weight[i] * (hit.emission[i] + hit.diffuse[i] * light.sun[i] * cosine / PI);
                weight[i] *= hit.diffuse[i];
            }
            ray = hemisphere(hit.normal, random(state), random(state));
        }
        if self.trace(origin, ray, f32::INFINITY).is_none() {
            for i in 0..3 {
                result[i] += weight[i] * light.sky[i];
            }
        }
        result
    }

    /// Integrate diffuse irradiance at surface samples. The receiver material
    /// and the sun's direct term are applied later by the renderer.
    pub fn bake_irradiance(
        &self,
        surfaces: &[SurfaceSample],
        lighting: Lighting,
        samples: u32,
    ) -> Result<Vec<Vec3>, BakeError> {
        let light = lighting.normalized()?;
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
            sky: [0.5, 0.25, 1.],
            sun: [10.; 3],
            direction_to_sun: [0., 1., 0.],
        };
        let actual = scene.bake_irradiance(&[surface()], l, 16).unwrap()[0];
        for (v, sky) in actual.into_iter().zip(l.sky) {
            assert!((v - PI * sky).abs() < 1e-5);
        }
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
            emission: [1., 0., 0.],
            base_texture: Some(0),
            alpha_cutoff: Some(0.5),
            ..Default::default()
        };
        let l = Lighting {
            sky: [0., 0., 1.],
            sun: [0.; 3],
            direction_to_sun: [0., 1., 0.],
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

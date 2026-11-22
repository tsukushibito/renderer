use crate::{BakeScene, Lighting, Vec3, cross, dot, unit};
use renderer::pbr::{ReflectionProbeDesc, ReflectionProbes};
use std::f32::consts::{PI, TAU};

const W: usize = ReflectionProbes::WIDTH;
const H: usize = ReflectionProbes::HEIGHT;

fn direction(x: usize, y: usize, height: usize) -> Vec3 {
    let (s, c) = (TAU * ((x as f32 + 0.5) / W as f32 - 0.5)).sin_cos();
    let (sin_lat, cos_lat) = (PI * (y as f32 + 0.5) / height as f32).sin_cos();
    [c * sin_lat, cos_lat, s * sin_lat]
}

fn sample(capture: &[Vec3], ray: Vec3) -> Vec3 {
    let u = (0.5 + ray[2].atan2(ray[0]) / TAU) * W as f32 - 0.5;
    let v = (ray[1].clamp(-1., 1.).acos() / PI * H as f32 - 0.5).clamp(0., (H - 1) as f32);
    let x = (u.floor() as i32).rem_euclid(W as i32) as usize;
    let y = v.floor() as usize;
    let mix =
        |a: Vec3, b: Vec3, t: f32| -> Vec3 { std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t) };
    let row = |y: usize| {
        mix(
            capture[y * W + x],
            capture[y * W + (x + 1) % W],
            u - u.floor(),
        )
    };
    mix(row(y), row((y + 1).min(H - 1)), v - v.floor())
}

// Importance sample GGX with N=V, weighted by N.L for the split-sum radiance term.
fn filter(capture: &[Vec3], n: Vec3, roughness: f32) -> Vec3 {
    let axis = if n[1].abs() < 0.99 {
        [0., 1., 0.]
    } else {
        [1., 0., 0.]
    };
    let tangent = unit(cross(axis, n)).unwrap();
    let bitangent = cross(n, tangent);
    let mut color = [0.; 3];
    let mut weight = 0.;
    for i in 0..64u32 {
        let u = i.reverse_bits() as f32 / 4294967296.;
        let (s, c) = (TAU * i as f32 / 64.).sin_cos();
        let cos = ((1. - u) / (1. + (roughness.powi(4) - 1.) * u)).sqrt();
        let sin = (1. - cos * cos).max(0.).sqrt();
        let half: Vec3 =
            std::array::from_fn(|j| n[j] * cos + sin * (c * tangent[j] + s * bitangent[j]));
        let ray = std::array::from_fn(|j| 2. * cos * half[j] - n[j]);
        let nl = dot(n, ray);
        if nl > 0. {
            let incoming = sample(capture, ray);
            for j in 0..3 {
                color[j] += incoming[j] * nl;
            }
            weight += nl;
        }
    }
    color.map(|v| v / weight)
}

impl BakeScene {
    /// Capture static transport for two lighting scenarios and GGX-filter each
    /// capture. Matched ray sequences keep scenario interpolation coherent.
    pub fn bake_reflection_probes(
        &self,
        probes: &[ReflectionProbeDesc],
        scenarios: [Lighting; 2],
    ) -> Result<ReflectionProbes, Box<dyn std::error::Error>> {
        if !(1..=ReflectionProbes::MAX_COUNT).contains(&probes.len()) {
            return Err("invalid reflection probe count".into());
        }
        for probe in probes {
            probe.validate()?;
        }
        let [a, b] = scenarios;
        let lights = [a.normalized()?, b.normalized()?];
        let mut bytes = Vec::with_capacity(W * ReflectionProbes::ROWS * 16 * probes.len());
        for p in probes {
            let start = bytes.len();
            for (values, w) in [(p.position, p.blend_distance), (p.min, 0.), (p.max, 0.)] {
                for v in values.into_iter().chain([w]) {
                    bytes.extend(v.to_le_bytes());
                }
            }
            bytes.resize(start + W * 16, 0);
            for light in lights {
                let capture: Vec<_> = (0..W * H)
                    .map(|i| {
                        let ray = direction(i % W, i / W, H);
                        let mut sum = [0.; 3];
                        for j in 0..8 {
                            let mut seed = (i as u32).wrapping_mul(8) + j;
                            let color = self.incoming(p.position, ray, light, &mut seed);
                            for k in 0..3 {
                                sum[k] += color[k] / 8.;
                            }
                        }
                        sum
                    })
                    .collect();
                for level in 0..ReflectionProbes::LEVELS {
                    let height = H >> level;
                    for i in 0..W * height {
                        let color = if level == 0 {
                            capture[i]
                        } else {
                            filter(
                                &capture,
                                direction(i % W, i / W, height),
                                level as f32 / (ReflectionProbes::LEVELS - 1) as f32,
                            )
                        };
                        for v in color.into_iter().chain([1.]) {
                            bytes.extend(v.to_le_bytes());
                        }
                    }
                }
            }
        }
        Ok(ReflectionProbes::from_rgba32f(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn increasing_roughness_spreads_a_compact_highlight() {
        let direction_to_emitter = [1., 0., 0.];
        let capture: Vec<_> = (0..W * H)
            .map(|i| {
                if dot(direction(i % W, i / W, H), direction_to_emitter) > 0.96 {
                    [8.; 3]
                } else {
                    [0.; 3]
                }
            })
            .collect();
        let off_axis = unit([1., 0.5, 0.]).unwrap();
        let sharp = filter(&capture, direction_to_emitter, 0.08)[0];
        let rough = filter(&capture, direction_to_emitter, 0.7)[0];
        assert!(sharp > 2. * rough);
        assert!(filter(&capture, off_axis, 0.7)[0] > filter(&capture, off_axis, 0.08)[0]);
    }
    #[test]
    fn sky_energy_survives_every_filter_level_and_asset_roundtrip() {
        let scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        let p = ReflectionProbeDesc {
            position: [0.; 3],
            min: [-1.; 3],
            max: [1.; 3],
            blend_distance: 0.5,
        };
        let sky = |v| Lighting {
            sky: [v; 3],
            sun: [0.; 3],
            direction_to_sun: [0., 1., 0.],
        };
        let asset = scene
            .bake_reflection_probes(&[p], [sky(0.25), sky(2.)])
            .unwrap();
        let mut bytes = vec![];
        asset.write(&mut bytes).unwrap();
        let asset = ReflectionProbes::from_bytes(&bytes).unwrap();
        let scenario = W * (2 * H - 1) * 16;
        for (data, expected) in asset.rgba32f()[W * 16..]
            .chunks_exact(scenario)
            .zip([0.25, 2.])
        {
            for texel in data.as_chunks::<16>().0 {
                assert!(
                    (f32::from_le_bytes(texel[..4].try_into().unwrap()) - expected).abs() < 1e-5
                );
            }
        }
        assert!(ReflectionProbes::from_bytes(&bytes[..bytes.len() - 16]).is_err());
    }
}

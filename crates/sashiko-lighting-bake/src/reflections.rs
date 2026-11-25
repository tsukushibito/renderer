use crate::{BakeScene, Vec3, cross, dot, unit};
use sashiko_renderer::pbr::{ReflectionProbeDesc, ReflectionProbes};
use std::f32::consts::{PI, TAU};

const W: usize = ReflectionProbes::WIDTH;
const H: usize = ReflectionProbes::HEIGHT;

pub(crate) fn direction(x: usize, y: usize, width: usize, height: usize) -> Vec3 {
    let (s, c) = (TAU * ((x as f32 + 0.5) / width as f32 - 0.5)).sin_cos();
    let (sin_lat, cos_lat) = (PI * (y as f32 + 0.5) / height as f32).sin_cos();
    [c * sin_lat, cos_lat, s * sin_lat]
}

pub(crate) fn sample(capture: &[Vec3], width: usize, ray: Vec3) -> Vec3 {
    let height = capture.len() / width;
    let u = (0.5 + ray[2].atan2(ray[0]) / TAU) * width as f32 - 0.5;
    let v =
        (ray[1].clamp(-1., 1.).acos() / PI * height as f32 - 0.5).clamp(0., (height - 1) as f32);
    let x = (u.floor() as i32).rem_euclid(width as i32) as usize;
    let y = v.floor() as usize;
    let mix =
        |a: Vec3, b: Vec3, t: f32| -> Vec3 { std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t) };
    let row = |y: usize| {
        mix(
            capture[y * width + x],
            capture[y * width + (x + 1) % width],
            u - u.floor(),
        )
    };
    mix(row(y), row((y + 1).min(height - 1)), v - v.floor())
}

// Importance sample GGX with N=V, weighted by N.L for the split-sum radiance term.
pub(crate) fn filter(capture: &[Vec3], width: usize, n: Vec3, roughness: f32) -> Vec3 {
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
            let incoming = sample(capture, width, ray);
            for j in 0..3 {
                color[j] += incoming[j] * nl;
            }
            weight += nl;
        }
    }
    color.map(|v| v / weight)
}

pub(crate) fn parallel_fill<T: Send>(
    output: &mut [T],
    min_chunk: usize,
    sample: impl Fn(usize) -> T + Sync,
) {
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    let chunk = output.len().div_ceil(workers).max(min_chunk);
    std::thread::scope(|scope| {
        for (i, pixels) in output.chunks_mut(chunk).enumerate() {
            let sample = &sample;
            scope.spawn(move || {
                for (j, value) in pixels.iter_mut().enumerate() {
                    *value = sample(i * chunk + j);
                }
            });
        }
    });
}

impl BakeScene {
    /// Capture static transport for one lighting condition or two endpoints and GGX-filter each
    /// capture. Matched ray sequences keep scenario interpolation coherent.
    pub fn bake_reflection_probes<const N: usize>(
        &self,
        probes: &[ReflectionProbeDesc],
        scenarios: impl crate::LightingScenarios<N>,
    ) -> Result<ReflectionProbes, Box<dyn std::error::Error>> {
        if !(1..=ReflectionProbes::MAX_COUNT).contains(&probes.len()) {
            return Err("invalid reflection probe count".into());
        }
        for probe in probes {
            probe.validate()?;
        }
        if N > 2 {
            return Err("reflection probes support at most two scenarios".into());
        }
        let lights = self.lighting(scenarios)?;
        let mut bytes =
            Vec::with_capacity(W * ReflectionProbes::SCENARIO_ROWS * N * 16 * probes.len());
        let mut capture = vec![[0.; 3]; W * H];
        let mut filtered = vec![[0.; 3]; W * (H / 2)];
        for p in probes {
            for light in lights {
                parallel_fill(&mut capture, 128, |i| {
                    let ray = direction(i % W, i / W, W, H);
                    let mut sum = [0.; 3];
                    for j in 0..8 {
                        let mut seed = i as u32 * 8 + j;
                        let color = self.incoming(p.position, ray, light, &mut seed);
                        for k in 0..3 {
                            sum[k] += color[k] / 8.;
                        }
                    }
                    sum
                });
                for level in 0..ReflectionProbes::LEVELS {
                    let height = H >> level;
                    let colors = if level == 0 {
                        capture.as_slice()
                    } else {
                        let values = &mut filtered[..W * height];
                        parallel_fill(values, 128, |i| {
                            filter(
                                &capture,
                                W,
                                direction(i % W, i / W, W, height),
                                level as f32 / (ReflectionProbes::LEVELS - 1) as f32,
                            )
                        });
                        &*values
                    };
                    for color in colors {
                        for v in color.iter().copied().chain([1.]) {
                            bytes.extend(v.to_le_bytes());
                        }
                    }
                    let last_row = bytes.len() - W * 16..bytes.len();
                    for _ in height..height.next_multiple_of(4) {
                        bytes.extend_from_within(last_row.clone());
                    }
                }
            }
        }
        Ok(ReflectionProbes::new(
            probes.to_vec(),
            N,
            sashiko_renderer::core::ImageFormat::Rgba32Float,
            bytes,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lighting;
    #[test]
    fn increasing_roughness_spreads_a_compact_highlight() {
        let direction_to_emitter = [1., 0., 0.];
        let capture: Vec<_> = (0..W * H)
            .map(|i| {
                if dot(direction(i % W, i / W, W, H), direction_to_emitter) > 0.96 {
                    [8.; 3]
                } else {
                    [0.; 3]
                }
            })
            .collect();
        let off_axis = unit([1., 0.5, 0.]).unwrap();
        let sharp = filter(&capture, W, direction_to_emitter, 0.08)[0];
        let rough = filter(&capture, W, direction_to_emitter, 0.7)[0];
        assert!(sharp > 2. * rough);
        assert!(filter(&capture, W, off_axis, 0.7)[0] > filter(&capture, W, off_axis, 0.08)[0]);
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
            environment: None,
            sky_rgb_nits: [v; 3],
            sun: crate::Sun::OFF,
        };
        let asset = scene
            .bake_reflection_probes(&[p], [sky(0.25), sky(2.)])
            .unwrap();
        let mut bytes = vec![];
        asset.write(&mut bytes).unwrap();
        let asset = ReflectionProbes::from_bytes(&bytes).unwrap();
        let scenario = W * ReflectionProbes::SCENARIO_ROWS * 16;
        for (data, expected) in asset.pixels().chunks_exact(scenario).zip([0.25, 2.]) {
            for texel in data.as_chunks::<16>().0 {
                assert!(
                    (f32::from_le_bytes(texel[..4].try_into().unwrap()) - expected).abs() < 1e-5
                );
            }
        }
        assert!(ReflectionProbes::from_bytes(&bytes[..bytes.len() - 16]).is_err());
    }
}

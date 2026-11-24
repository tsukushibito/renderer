use crate::{BakeError, BakeScene, Lighting, ProbeShOrder, Vec3};
use renderer::pbr::BakedLighting;
use std::{
    collections::HashMap,
    f32::consts::{PI, TAU},
};

/// Controls placement and diffuse irradiance integration for a probe volume.
#[derive(Clone, Copy)]
pub struct ProbeSettings {
    pub backend: crate::BakeBackend,
    /// Highest spherical harmonic degree retained at each probe, across all scenarios.
    pub sh_order: ProbeShOrder,
    /// Smallest desired spacing near geometry. The root also covers an empty margin.
    pub spacing: f32,
    pub max_depth: u8,
    pub rays_per_probe: u32,
}
impl Default for ProbeSettings {
    fn default() -> Self {
        Self {
            backend: Default::default(),
            sh_order: ProbeShOrder::L1,
            spacing: 2.,
            max_depth: 5,
            rays_per_probe: 128,
        }
    }
}
#[derive(Clone, Copy)]
struct Cell {
    origin: [u32; 3],
    span: u32,
}
fn corner(i: usize) -> [u32; 3] {
    std::array::from_fn(|axis| ((i >> axis) & 1) as u32)
}
fn coords(base: [u32; 3], span: u32, i: usize) -> [u32; 3] {
    let c = corner(i);
    std::array::from_fn(|a| base[a] + span * c[a])
}

// Cartesian weights for the cosine-convolved real SH projection of one ray.
fn linear_sh([x, y, z]: Vec3) -> [f32; 4] {
    [1., 2. * x, 2. * y, 2. * z]
}

fn quadratic_sh(dir: Vec3) -> [f32; 9] {
    let [x, y, z] = dir;
    let [dc, lx, ly, lz] = linear_sh(dir);
    // The five additional terms sum to (5/8) * (3 * dot(dir, normal)^2 - 1)
    // when evaluated against xy, yz, xz, x²-y² and 3z²-1 of the unit normal.
    [
        dc,
        lx,
        ly,
        lz,
        3.75 * x * y,
        3.75 * y * z,
        3.75 * x * z,
        0.9375 * (x * x - y * y),
        0.3125 * (3. * z * z - 1.),
    ]
}

impl BakeScene {
    fn project_probe<const K: usize>(
        &self,
        position: Vec3,
        lights: &[Lighting],
        samples: u32,
        coefficients: &mut [[f32; 4]],
        basis: impl Fn(Vec3) -> [f32; K],
    ) {
        let sample_weight = PI / samples as f32;
        let (scenarios, _) = coefficients.as_chunks_mut::<K>();
        for j in 0..samples {
            let z = 1. - 2. * (j as f32 + 0.5) / samples as f32;
            let r = (1. - z * z).max(0.).sqrt();
            let (s, c) = (TAU * (j.reverse_bits() as f64 / 4294967296.) as f32).sin_cos();
            let dir = [r * c, z, r * s];
            let weights = basis(dir);
            for (&light, dst) in lights.iter().zip(scenarios.iter_mut()) {
                let mut seed = j;
                let radiance = self.incoming(position, dir, light, &mut seed);
                for (coefficient, weight) in dst.iter_mut().zip(weights) {
                    for (channel, l) in radiance.into_iter().enumerate() {
                        coefficient[channel] += l * sample_weight * weight;
                    }
                }
            }
        }
    }

    fn intersects_geometry(&self, p: Vec3, size: f32) -> bool {
        let mut i = 0;
        while let Some(node) = self.bvh.0.get(i) {
            if (0..3).any(|a| node.lo[a] > p[a] + size || node.hi[a] < p[a]) {
                i = node.escape as usize;
            } else if node.count > 0 {
                return true;
            } else {
                i += 1;
            }
        }
        false
    }

    /// Use signed first hits on six axes to move a probe off a nearby wall.
    /// Deeply buried probes are excluded; they cannot be relocated within
    /// their local interpolation cell.
    fn place_probe(&self, p: Vec3, cell: f32) -> (Vec3, bool) {
        let clearance = cell * 0.06;
        let mut backfaces = 0;
        let mut nearest_back = None;
        let mut nearest_front = None;
        for axis in 0..3 {
            for sign in [-1., 1.] {
                let mut d = [0.; 3];
                d[axis] = sign;
                if let Some(hit) = self.trace(p, d, f32::INFINITY) {
                    let candidate = if hit.backface {
                        backfaces += 1;
                        &mut nearest_back
                    } else {
                        &mut nearest_front
                    };
                    if candidate
                        .as_ref()
                        .is_none_or(|(dist, _)| hit.distance < *dist)
                    {
                        *candidate = Some((hit.distance, d));
                    }
                }
            }
        }
        let (distance, direction) = if backfaces >= 4 {
            let (d, ray) = nearest_back.expect("backface hits counted");
            if d + clearance > cell * 0.35 {
                return ([0.; 3], false);
            }
            (d + clearance, ray)
        } else if let Some((d, ray)) = nearest_front {
            ((clearance - d).max(0.), ray.map(|v| -v))
        } else {
            return ([0.; 3], true);
        };
        (direction.map(|v| v * distance), true)
    }
    /// Bake equally spaced lighting scenarios into one atlas. All corners shared by adjacent
    /// leaves are integrated once, including corners at different resolutions.
    pub fn bake_probes<const N: usize>(
        &self,
        scenarios: impl crate::LightingScenarios<N>,
        settings: ProbeSettings,
    ) -> Result<BakedLighting, Box<dyn std::error::Error>> {
        let lights = self.lighting(scenarios)?;
        if !settings.spacing.is_normal()
            || settings.spacing <= 0.
            || settings.max_depth as usize > BakedLighting::MAX_DEPTH
            || settings.rays_per_probe == 0
        {
            return Err(BakeError("invalid probe bake settings").into());
        }
        let bounds = self.bvh.0.first();
        let lo = bounds.map_or([0.; 3], |n| n.lo);
        let hi = bounds.map_or([0.; 3], |n| n.hi);
        let length = (0..3).map(|i| hi[i] - lo[i]).fold(0f32, f32::max) + 2. * settings.spacing;
        let origin: Vec3 = std::array::from_fn(|i| lo[i] + ((hi[i] - lo[i]) - length) * 0.5);
        if !length.is_finite() || !crate::finite(origin) {
            return Err(BakeError("unrepresentable bake bounds").into());
        }
        let divisions = 1u32 << settings.max_depth;
        let scale = length / divisions as f32;
        let position = |key: [u32; 3]| std::array::from_fn(|a| origin[a] + scale * key[a] as f32);
        let mut cells = vec![Cell {
            origin: [0; 3],
            span: divisions,
        }];
        let mut records = Vec::new();
        let mut positions = Vec::new();
        let mut ids = HashMap::new();
        let mut spacings = Vec::new();
        let stride = 2 + settings.sh_order.coefficient_count() * N;
        let mut index = 0;
        while index < cells.len() {
            let cell = cells[index];
            let size = cell.span as f32 * scale;
            let mut node = [[0.; 4]; 3];
            if cell.span > 1
                && size > settings.spacing
                && self.intersects_geometry(position(cell.origin), size)
            {
                node[0][0] = cells.len() as f32;
                let span = cell.span / 2;
                cells.extend((0..8).map(|i| Cell {
                    origin: coords(cell.origin, span, i),
                    span,
                }));
            } else {
                for i in 0..8 {
                    let key = coords(cell.origin, cell.span, i);
                    let id = ids.entry(key).or_insert_with(|| {
                        positions.push(position(key));
                        spacings.push(size);
                        positions.len() - 1
                    });
                    spacings[*id] = spacings[*id].min(size);
                    node[1 + i / 4][i % 4] = *id as f32;
                }
            }
            if 2 + N.max(2) + cells.len() * 3 + positions.len() * stride > 256 * 16384 {
                return Err(BakeError(
                    "probe atlas exceeds capacity; increase spacing or reduce depth",
                )
                .into());
            }
            records.extend(node);
            index += 1;
        }
        let mut texels = vec![
            [origin[0], origin[1], origin[2], length],
            [
                records.len() as f32 / 3.,
                positions.len() as f32,
                0.,
                settings.sh_order.atlas_tag(N),
            ],
        ];
        for i in 0..N.max(2) {
            let sky = self.mean_sky(lights[i.min(N - 1)]);
            texels.push([PI * sky[0], PI * sky[1], PI * sky[2], 0.]);
        }
        texels.extend(records);
        let offsets = positions
            .iter_mut()
            .zip(spacings)
            .map(|(p, span)| {
                let (delta, valid) = self.place_probe(*p, span);
                *p = std::array::from_fn(|i| p[i] + delta[i]);
                (delta, valid)
            })
            .collect::<Vec<_>>();
        let probe_start = texels.len();
        let jobs = positions
            .iter()
            .map(|&position| crate::SurfaceSample {
                position,
                normal: [0.; 3],
            })
            .collect::<Vec<_>>();
        if let Some(result) = settings.backend.integrate(
            self,
            &jobs,
            lights,
            settings.rays_per_probe,
            length,
            settings.sh_order,
        )? {
            texels.extend(result);
        } else {
            // Independent receiver work, with fixed per-probe summation order.
            let start = texels.len();
            texels.resize(start + positions.len() * stride, [0.; 4]);
            let threads = std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(positions.len());
            let chunk = positions.len().div_ceil(threads);
            std::thread::scope(|scope| {
                for (receivers, out) in positions
                    .chunks(chunk)
                    .zip(texels[start..].chunks_mut(chunk * stride))
                {
                    scope.spawn(move || {
                        for (&p, data) in receivers.iter().zip(out.chunks_exact_mut(stride)) {
                            for axis in 0..3 {
                                for (side, sign) in [(0, -1.), (1, 1.)] {
                                    let mut d = [0.; 3];
                                    d[axis] = sign;
                                    data[side][axis] =
                                        self.trace(p, d, length).map_or(length, |h| h.distance);
                                }
                            }
                            let coefficients = &mut data[2..];
                            match settings.sh_order {
                                ProbeShOrder::L1 => self.project_probe(
                                    p,
                                    &lights,
                                    settings.rays_per_probe,
                                    coefficients,
                                    linear_sh,
                                ),
                                ProbeShOrder::L2 => self.project_probe(
                                    p,
                                    &lights,
                                    settings.rays_per_probe,
                                    coefficients,
                                    quadratic_sh,
                                ),
                            }
                        }
                    });
                }
            });
        }
        for ((offset, valid), p) in offsets
            .into_iter()
            .zip(texels[probe_start..].chunks_exact_mut(stride))
        {
            p[0][3] = f32::from(valid);
            for i in 0..3 {
                p[2 + i][3] = offset[i];
            }
        }
        let bytes = texels
            .into_iter()
            .flatten()
            .flat_map(f32::to_le_bytes)
            .collect();
        BakedLighting::from_texels(bytes)
            .map_err(|_| BakeError("invalid computed probe atlas").into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quadratic_sky_recovers_analytic_irradiance_and_retains_l1_coefficients() {
        // For sky radiance 1 + dot(axis, ray)^2, cosine integration over the
        // receiving hemisphere gives E/pi = 1.25 + 0.25 * dot(axis, normal)^2.
        let axis = crate::unit([2., 3., -4.]).unwrap();
        let (width, height) = (128, 64);
        let mut sky = Vec::with_capacity(width * height);
        for row in 0..height {
            let (r, y) = (PI * (row as f32 + 0.5) / height as f32).sin_cos();
            for col in 0..width {
                let (s, c) = (TAU * ((col as f32 + 0.5) / width as f32 - 0.5)).sin_cos();
                sky.push([1. + crate::dot(axis, [r * c, y, r * s]).powi(2); 4]);
            }
        }
        let mut scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        let light = Lighting {
            sun: crate::Sun::OFF,
            sky_rgb_nits: [1.; 3],
            environment: Some(
                scene
                    .add_sky_map(crate::Texture::new(width, height, sky).unwrap(), 0.)
                    .unwrap(),
            ),
        };
        let settings = ProbeSettings {
            backend: crate::BakeBackend::Cpu,
            rays_per_probe: 4096,
            ..Default::default()
        };
        let l1 = scene.bake_probes(light, settings).unwrap();
        let l2 = scene
            .bake_probes(
                light,
                ProbeSettings {
                    sh_order: ProbeShOrder::L2,
                    ..settings
                },
            )
            .unwrap();
        assert_eq!(l1.sh_order(), ProbeShOrder::L1);
        assert_eq!(l2.sh_order(), ProbeShOrder::L2);
        // Single-key, one-leaf volume: 4 header, 3 node and 2 visibility texels.
        let start = 9 * 16;
        assert_eq!(
            &l1.rgba32f()[start..start + 4 * 16],
            &l2.rgba32f()[start..start + 4 * 16]
        );
        let red: [f32; 9] = std::array::from_fn(|i| {
            let at = start + i * 16;
            f32::from_le_bytes(l2.rgba32f()[at..at + 4].try_into().unwrap())
        });
        for n in [
            axis,
            [1., 0., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            crate::unit([1., -1., 1.]).unwrap(),
            crate::unit([3., -2., 0.]).unwrap(),
        ] {
            let [x, y, z] = n;
            let basis = [
                1.,
                x,
                y,
                z,
                x * y,
                y * z,
                x * z,
                x * x - y * y,
                3. * z * z - 1.,
            ];
            let measured = red.iter().zip(basis).map(|(c, b)| c * b).sum::<f32>() / PI;
            let expected = 1.25 + 0.25 * crate::dot(axis, n).powi(2);
            assert!(
                (expected - measured).abs() < 0.002,
                "n={n:?}, expected {expected}, measured {measured}"
            );
        }
    }

    #[test]
    fn sky_scenarios_share_a_leaf_and_survive_serialization() {
        let scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        let sky = Lighting {
            sun: crate::Sun::OFF,
            environment: None,
            sky_rgb_nits: [1.; 3],
        };
        let volume = scene
            .bake_probes(
                [
                    sky,
                    Lighting {
                        sky_rgb_nits: [0.25; 3],
                        ..sky
                    },
                ],
                ProbeSettings {
                    backend: crate::BakeBackend::Cpu,
                    ..Default::default()
                },
            )
            .unwrap();
        let mut file = vec![];
        volume.write(&mut file).unwrap();
        assert_eq!(
            BakedLighting::from_bytes(&file).unwrap().rgba32f(),
            volume.rgba32f()
        );
        let read = |t: usize| f32::from_le_bytes(file[8 + 16 * t..12 + 16 * t].try_into().unwrap());
        assert_eq!(read(1), 1.);
        assert!((read(9) - PI).abs() < 1e-5);
        assert!((read(13) - 0.25 * PI).abs() < 1e-5);
        // Invalid probe reference in leaf zero.
        file[88..92].copy_from_slice(&99f32.to_le_bytes());
        assert!(BakedLighting::from_bytes(&file).is_err());
    }
}

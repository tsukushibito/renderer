use crate::{BakeError, BakeScene, Vec3};
use renderer::pbr::BakedLighting;
use std::{
    collections::HashMap,
    f32::consts::{PI, TAU},
};

/// Smallest desired spacing near geometry. The root also covers an empty margin.
#[derive(Clone, Copy)]
pub struct ProbeSettings {
    pub backend: crate::BakeBackend,
    pub spacing: f32,
    pub max_depth: u8,
    pub rays_per_probe: u32,
}
impl Default for ProbeSettings {
    fn default() -> Self {
        Self {
            backend: Default::default(),
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

fn probe_ray(index: u32, count: u32) -> Vec3 {
    let z = 1. - 2. * (index as f32 + 0.5) / count as f32;
    let r = (1. - z * z).max(0.).sqrt();
    let (s, c) = (TAU * (index.reverse_bits() as f64 / 4294967296.) as f32).sin_cos();
    [r * c, z, r * s]
}

fn linear_sh([x, y, z]: Vec3) -> [f32; 4] {
    [1., 2. * x, 2. * y, 2. * z]
}

// In this Cartesian basis the common sample weight is pi / ray count.
// The L2 contribution to the clamped cosine kernel is (5/8)(3 dot(w,n)^2 - 1).
fn quadratic_sh([x, y, z]: Vec3) -> [f32; 5] {
    [
        3.75 * x * y,
        3.75 * y * z,
        3.75 * x * z,
        0.9375 * (x * x - y * y),
        0.3125 * (3. * z * z - 1.),
    ]
}

impl BakeScene {
    /// Offline comparison at supplied world-space probe positions. The first four
    /// coefficients use the production CPU bake's rays, weights and summation order.
    /// Evaluate RGB irradiance with [1, x, y, z, xy, yz, xz, x²-y², 3z²-1] for
    /// a unit surface normal (x,y,z). The result already includes the cosine integral.
    /// Optional ray output contains little-endian f32 triples in this order:
    /// all world-space directions, then incident RGB radiance at each position
    /// in input order and, within each position, the same direction order.
    pub fn probe_sh_reference(
        &self,
        positions: &[Vec3],
        light: crate::Lighting,
        samples: u32,
        mut ray_output: Option<&mut dyn std::io::Write>,
    ) -> Result<Vec<[Vec3; 9]>, Box<dyn std::error::Error>> {
        light.validate(self)?;
        if samples == 0 || positions.iter().any(|&p| !crate::finite(p)) {
            return Err(BakeError("invalid probe reference position or sample count").into());
        }
        fn write_triple(output: &mut dyn std::io::Write, v: Vec3) -> std::io::Result<()> {
            for component in v {
                output.write_all(&component.to_le_bytes())?;
            }
            Ok(())
        }
        let capture = ray_output.is_some();
        if let Some(output) = &mut ray_output {
            for j in 0..samples {
                write_triple(*output, probe_ray(j, samples))?;
            }
        }
        let workers = std::thread::available_parallelism().map_or(1, usize::from);
        // With a dump, keep at most 16 MiB of pending ray data or one probe.
        let batch_size = if capture {
            (16 * 1024 * 1024 / size_of::<Vec3>() / samples as usize).max(1)
        } else {
            positions.len().max(1)
        };
        let mut result = vec![[[0.; 3]; 9]; positions.len()];
        for (batch, out) in positions
            .chunks(batch_size)
            .zip(result.chunks_mut(batch_size))
        {
            let chunk = batch.len().div_ceil(workers);
            std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
                let mut jobs = Vec::new();
                for (positions, coefficients) in batch.chunks(chunk).zip(out.chunks_mut(chunk)) {
                    jobs.push(scope.spawn(move || {
                        let mut radiances = Vec::new();
                        if capture {
                            radiances.try_reserve_exact(positions.len() * samples as usize)?;
                        }
                        for (&position, sh) in positions.iter().zip(coefficients) {
                            for j in 0..samples {
                                let dir = probe_ray(j, samples);
                                let mut seed = j;
                                let radiance = self.incoming(position, dir, light, &mut seed);
                                if capture {
                                    radiances.push(radiance);
                                }
                                let basis = linear_sh(dir).into_iter().chain(quadratic_sh(dir));
                                for (coefficient, weight) in sh.iter_mut().zip(basis) {
                                    for (channel, l) in radiance.into_iter().enumerate() {
                                        coefficient[channel] += l * (PI / samples as f32) * weight;
                                    }
                                }
                            }
                        }
                        Ok::<_, std::collections::TryReserveError>(radiances)
                    }));
                }
                for job in jobs {
                    let radiances = job.join().expect("probe worker panicked")?;
                    if let Some(output) = &mut ray_output {
                        for v in radiances {
                            write_triple(*output, v)?;
                        }
                    }
                }
                Ok(())
            })?;
        }
        Ok(result)
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
            if 2 + N.max(2) + cells.len() * 3 + positions.len() * (2 + 4 * N) > 256 * 16384 {
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
                if N == 2 { 0. } else { N as f32 },
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
        let stride = 2 + 4 * N;
        let jobs = positions
            .iter()
            .map(|&position| crate::SurfaceSample {
                position,
                normal: [0.; 3],
            })
            .collect::<Vec<_>>();
        if let Some(result) =
            settings
                .backend
                .integrate(self, &jobs, lights, settings.rays_per_probe, length)?
        {
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
                            for j in 0..settings.rays_per_probe {
                                let dir = probe_ray(j, settings.rays_per_probe);
                                for (scenario, light) in lights.iter().enumerate() {
                                    let mut seed = j;
                                    let radiance = self.incoming(p, dir, *light, &mut seed);
                                    // Cosine-convolved real SH in Cartesian form.
                                    for (k, basis) in linear_sh(dir).into_iter().enumerate() {
                                        for (channel, l) in radiance.into_iter().enumerate() {
                                            data[2 + 4 * scenario + k][channel] +=
                                                l * (PI / settings.rays_per_probe as f32) * basis;
                                        }
                                    }
                                }
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
    use crate::Lighting;
    #[test]
    fn ray_dump_matches_the_projected_transport_and_input_order() {
        let scene = BakeScene::new(
            vec![crate::Triangle {
                positions: [[-2., 0., -2.], [0., 0., 2.], [2., 0., -2.]],
                material: 0,
                uv: [[0.; 2]; 3],
            }],
            vec![crate::Material {
                diffuse: [0.; 3],
                emissive_nits: [1., 2., 3.],
                ..Default::default()
            }],
            vec![],
        )
        .unwrap();
        let light = Lighting {
            sun: crate::Sun::OFF,
            environment: None,
            sky_rgb_nits: [0.01; 3],
        };
        let positions = [[0., 0.3, 0.], [42., 3., 1.]];
        let samples = 31;
        let mut raw = Vec::new();
        let sh = scene
            .probe_sh_reference(&positions, light, samples, Some(&mut raw))
            .unwrap();
        assert_eq!(
            sh,
            scene
                .probe_sh_reference(&positions, light, samples, None)
                .unwrap()
        );
        assert_ne!(sh[0][0], sh[1][0]);
        let triples: Vec<Vec3> = raw
            .as_chunks::<12>()
            .0
            .iter()
            .map(|bytes| {
                std::array::from_fn(|i| {
                    f32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().unwrap())
                })
            })
            .collect();
        assert_eq!(raw.len(), (positions.len() + 1) * samples as usize * 12);
        for j in 0..samples {
            let dir = probe_ray(j, samples);
            assert_eq!(triples[j as usize], dir);
            for (i, p) in positions.iter().enumerate() {
                let mut seed = j;
                assert_eq!(
                    triples[(i + 1) * samples as usize + j as usize],
                    scene.incoming(*p, dir, light, &mut seed)
                );
            }
        }
    }

    #[test]
    fn quadratic_projection_recovers_rotated_legendre_kernel() {
        for ray in 0..17 {
            let dir = probe_ray(ray, 17);
            for receiver in 0..19 {
                let n @ [x, y, z] = probe_ray(receiver, 19);
                let basis = [x * y, y * z, x * z, x * x - y * y, 3. * z * z - 1.];
                let fitted: f32 = quadratic_sh(dir)
                    .into_iter()
                    .zip(basis)
                    .map(|(a, b)| a * b)
                    .sum();
                let dot = crate::dot(dir, n);
                assert!((fitted - 0.625 * (3. * dot * dot - 1.)).abs() < 1e-6);
            }
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

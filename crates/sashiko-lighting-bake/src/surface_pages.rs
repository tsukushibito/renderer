//! Offline allocation and rasterization of cooked lightmap UVs. A placement owns
//! its rectangle(s), even when several placements use identical geometry and UVs.
mod packing;

use crate::{
    BakeError, BakeScene, SurfaceBake, SurfaceSample, Vec3, cross, dot, finite, sub, unit,
};
use sashiko_renderer::pbr::SurfacePageRecord;
use std::{collections::HashSet, error::Error};

/// Measured change-boundary costs for a single daylight bake.
#[derive(Default)]
pub struct BakeBenchmark {
    pub pages: Vec<BakePageTiming>,
    pub transport_setup: std::time::Duration,
    pub probes: std::time::Duration,
    pub write_pages: std::time::Duration,
}
/// Samples in deterministic receiver order. GPU and wait durations overlap elapsed time.
pub struct BakePageTiming {
    pub raster: std::time::Duration,
    pub gpu: Option<std::time::Duration>,
    pub host_wait: std::time::Duration,
    pub host_transfer: std::time::Duration,
    pub receivers: usize,
    pub elapsed: std::time::Duration,
    pub irradiance: Vec<Vec3>,
}

#[derive(Clone, Copy, Debug)]
pub struct LightmapVertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub material_uv: [f32; 2],
    pub lightmap_uv: [f32; 2],
}

/// Positions and normals are in bake-world space. Extents include the chart
/// padding from the unwrap and are measured at the placement's world scale.
pub struct StaticReceiver {
    pub placement: u32,
    pub vertices: Vec<LightmapVertex>,
    pub indices: Vec<u32>,
    pub extent: [u32; 2],
    pub material: usize,
    pub double_sided: bool,
}
impl StaticReceiver {
    /// Place a cooked local mesh, sharing its normalized UV layout across placements.
    /// The extent is in texels at the mesh's original scale. A bound on the
    /// largest stretch of the model matrix preserves density after scaling/shear.
    pub fn from_mesh(
        placement: u32,
        vertices: &[LightmapVertex],
        indices: &[u32],
        extent: [u32; 2],
        material: usize,
        double_sided: bool,
        transform: sashiko_renderer::pbr::Transform,
    ) -> Result<Self, BakeError> {
        let matrix = transform.columns();
        let columns = [0, 1, 2].map(|i| [0, 1, 2].map(|j| f64::from(matrix[i][j])));
        // Largest eigenvalue of A^T A is bounded by its maximum absolute row sum.
        let stretch = columns
            .iter()
            .map(|a| {
                columns
                    .iter()
                    .map(|b| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>().abs())
                    .sum::<f64>()
            })
            .fold(0., f64::max)
            .sqrt();
        let scaled = extent.map(|n| (f64::from(n) * stretch).ceil().max(1.));
        if extent.contains(&0) || scaled.iter().any(|&n| n > f64::from(u32::MAX)) {
            return Err(BakeError("invalid scaled lightmap extent"));
        }
        let vertices = vertices
            .iter()
            .map(|v| {
                Ok(LightmapVertex {
                    position: transform.transform_point(v.position),
                    normal: unit(transform.transform_normal(v.normal))?,
                    ..*v
                })
            })
            .collect::<Result<_, BakeError>>()?;
        Ok(Self {
            placement,
            vertices,
            indices: indices.to_vec(),
            extent: scaled.map(|n| n as u32),
            material,
            double_sided,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceRegion {
    pub placement: u32,
    pub page: usize,
    pub back: bool,
    pub origin: [u32; 2],
    pub extent: [u32; 2],
}

pub struct SurfaceLayout<'a> {
    receivers: &'a [StaticReceiver],
    regions: Vec<(usize, SurfaceRegion)>,
    pages: Vec<[u32; 2]>,
    gpu_pages: Vec<GpuPageLayout>,
    scenarios: usize,
    page_limit: u32,
    metrics: packing::Metrics,
}

struct PageSamples<const N: usize> {
    lookup: Vec<usize>,
    values: Vec<[[f32; 3]; N]>,
}

struct GpuPageLayout {
    front: usize,
    back: usize,
    charts: Vec<(SurfaceRegion, SurfaceRegion)>,
}

fn image_shape(charts: usize, pixels: u64, keys: usize) -> Option<[usize; 2]> {
    let header = charts.checked_mul(2)?.checked_add(1)?;
    if header > 1 << 24 {
        return None;
    }
    let texels = usize::try_from(pixels)
        .ok()?
        .checked_mul(keys)?
        .checked_add(header)?;
    let width = texels.min((texels.isqrt() + 1).next_power_of_two().clamp(1, 16384));
    let height = texels.div_ceil(width);
    (height <= 16384).then_some([width, height])
}

impl GpuPageLayout {
    /// GPU table addresses are f32 texels containing exact integer words.
    /// RGB irradiance follows the table, with one RGBA texel per lighting key.
    fn begin_image<const N: usize>(
        &self,
        pages: &[[u32; 2]],
    ) -> Result<([u32; 2], Vec<u8>), Box<dyn Error>> {
        let header = 1 + self.charts.len() * 2;
        if header > 1 << 24 {
            return Err("lightmap chart table exceeds exact float range".into());
        }
        let [fw, fh] = pages[self.front];
        let [bw, bh] = pages[self.back];
        let back_offset = if self.front == self.back { 0 } else { fw * fh };
        let pixels = if self.front == self.back {
            fw * fh
        } else {
            back_offset + bw * bh
        };
        let [width, height] = image_shape(self.charts.len(), u64::from(pixels), N)
            .ok_or("lightmap exceeds the maximum cooked image dimensions")?;
        let mut image = Vec::new();
        image.try_reserve_exact(width * height * 16)?;
        let mut write = |words: [u32; 4]| {
            image.extend(words.into_iter().flat_map(|v| (v as f32).to_le_bytes()));
        };
        write([self.charts.len() as u32, N as u32, header as u32, 0]);
        for (front, back) in &self.charts {
            write([
                front.origin[1] * fw + front.origin[0],
                fw,
                back_offset + back.origin[1] * bw + back.origin[0],
                bw,
            ]);
            write([front.extent[0], front.extent[1], 0, 0]);
        }
        Ok(([width as u32, height as u32], image))
    }
}

impl<'a> SurfaceLayout<'a> {
    /// Choose standard page dimensions and cover every supplied placement.
    pub fn pack(receivers: &'a [StaticReceiver], scenarios: usize) -> Result<Self, Box<dyn Error>> {
        Self::new(receivers, 2048, scenarios, None)
    }

    /// Pack full-density rectangles with a one-texel border on every side.
    /// The optional budget counts allocated page texels per lighting scenario.
    pub fn new(
        receivers: &'a [StaticReceiver],
        page_side: u32,
        scenarios: usize,
        budget: Option<u64>,
    ) -> Result<Self, Box<dyn Error>> {
        if !(4..=2048).contains(&page_side) {
            return Err("invalid lightmap page size".into());
        }
        if !(1..=3).contains(&scenarios) {
            return Err("expected one to three lighting scenarios".into());
        }
        let mut ids = HashSet::new();
        for r in receivers {
            if !ids.insert(r.placement)
                || r.indices.is_empty()
                || !r.indices.len().is_multiple_of(3)
                || r.indices.iter().any(|&i| i as usize >= r.vertices.len())
                || r.vertices.iter().any(|v| {
                    !finite(v.position)
                        || unit(v.normal).is_err()
                        || v.material_uv.iter().any(|x| !x.is_finite())
                        || v.lightmap_uv.iter().any(|x| !(0.0..=1.0).contains(x))
                })
            {
                return Err(format!("invalid lightmap placement {}", r.placement).into());
            }
            if r.extent.contains(&0) || r.extent.iter().any(|&x| x > page_side - 2) {
                return Err(format!(
                    "placement {} needs {:?} plus two border texels; page side {page_side}",
                    r.placement, r.extent
                )
                .into());
            }
        }
        packing::select(receivers, page_side, scenarios, budget)
    }

    pub fn page_limit(&self) -> u32 {
        self.page_limit
    }
    /// Total RGBA32F payload across all independent GPU pages.
    pub fn resident_bytes(&self) -> u64 {
        self.metrics.bytes
    }
    pub fn material_page_groups(&self) -> usize {
        self.metrics.groups
    }

    pub fn texels(&self) -> u64 {
        self.pages
            .iter()
            .map(|[w, h]| u64::from(*w) * u64::from(*h))
            .sum()
    }
    pub fn pages(&self) -> &[[u32; 2]] {
        &self.pages
    }
    pub fn regions(&self) -> impl Iterator<Item = SurfaceRegion> + '_ {
        self.regions.iter().map(|&(_, region)| region)
    }

    /// Number of independently uploadable images. Dedicated front/back rectangles
    /// share one image so shading a placement never requires a second page.
    pub fn gpu_page_count(&self) -> usize {
        self.gpu_pages.len()
    }

    /// Bake the selected GPU pages in request order, using the prepared
    /// transport for every page. Unselected pages incur no raster or trace work.
    pub fn bake_gpu_pages<const N: usize>(
        &self,
        bake: SurfaceBake<'_, N>,
        selected: &[usize],
        write: impl FnMut(usize, SurfacePageRecord, Vec<u8>) -> Result<(), Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        self.write_gpu_pages(bake, selected, None, write)
    }

    /// Measure the same raster, transport, packing and write path as a full bake.
    /// The output is benchmark scratch; only the sampled receivers contain valid light.
    pub fn benchmark_to(
        &self,
        scene: &BakeScene,
        light: crate::Lighting,
        settings: crate::ProbeSettings,
        mut scratch: impl std::io::Write,
        seed: u64,
    ) -> Result<BakeBenchmark, Box<dyn Error>> {
        let start = std::time::Instant::now();
        let mut bake =
            scene.prepare_surface_bake([light], settings.backend, settings.rays_per_probe)?;
        bake.transport.enable_timing()?;
        let mut benchmark = BakeBenchmark {
            transport_setup: start.elapsed(),
            ..Default::default()
        };
        let selected: Vec<_> = (0..self.gpu_page_count()).collect();
        self.write_gpu_pages(
            bake,
            &selected,
            Some((seed, &mut benchmark)),
            |_, _, image| Ok(scratch.write_all(&image)?),
        )?;
        let start = std::time::Instant::now();
        scene.bake_probes([light], settings)?.write(scratch)?;
        benchmark.probes = start.elapsed();
        Ok(benchmark)
    }

    fn write_gpu_pages<const N: usize>(
        &self,
        mut bake: SurfaceBake<'_, N>,
        selected: &[usize],
        mut benchmark: Option<(u64, &mut BakeBenchmark)>,
        mut write: impl FnMut(usize, SurfacePageRecord, Vec<u8>) -> Result<(), Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        if N != self.scenarios {
            return Err("scenario count differs from the prepared layout".into());
        }
        let mut seen = HashSet::with_capacity(selected.len());
        for &index in selected {
            if index >= self.gpu_page_count() || !seen.insert(index) {
                return Err(format!("invalid or repeated lightmap GPU page {index}").into());
            }
        }
        for &index in selected {
            let gpu = &self.gpu_pages[index];
            let (extent, mut image) = gpu.begin_image::<N>(&self.pages)?;
            for page in gpu.front..=gpu.back {
                let PageSamples { lookup, values } =
                    self.trace_page(&mut bake, page, &mut benchmark)?;
                let start = benchmark.as_ref().map(|_| std::time::Instant::now());
                for index in lookup {
                    let colors = if index == usize::MAX {
                        &[[0.; 3]; N]
                    } else {
                        &values[index]
                    };
                    for &[r, g, b] in colors {
                        image.extend([r, g, b, 0.].into_iter().flat_map(f32::to_le_bytes));
                    }
                }
                if let (Some(start), Some((_, b))) = (start, benchmark.as_mut()) {
                    b.write_pages += start.elapsed();
                }
            }
            let start = benchmark.as_ref().map(|_| std::time::Instant::now());
            image.resize(extent[0] as usize * extent[1] as usize * 16, 0);
            let placements = gpu.charts.iter().map(|(f, _)| f.placement).collect();
            let record = SurfacePageRecord::from_texels(N, extent, placements, &image)?;
            write(index, record, image)?;
            if let (Some(start), Some((_, b))) = (start, benchmark.as_mut()) {
                b.write_pages += start.elapsed();
            }
        }
        bake.finish()
    }

    fn trace_page<const N: usize>(
        &self,
        bake: &mut SurfaceBake<'_, N>,
        page: usize,
        benchmark: &mut Option<(u64, &mut BakeBenchmark)>,
    ) -> Result<PageSamples<N>, Box<dyn Error>> {
        let [width, height] = self.pages[page];
        let raster = benchmark.as_ref().map(|_| std::time::Instant::now());
        let mut lookup = vec![usize::MAX; (width * height) as usize];
        let mut jobs = Vec::new();
        let first = self.regions.partition_point(|(_, r)| r.page < page);
        for &(i, r) in self.regions[first..]
            .iter()
            .take_while(|(_, r)| r.page == page)
        {
            let scene = bake.scene();
            if self.receivers[i].material >= scene.materials.len() {
                return Err(format!("unknown bake material on placement {}", r.placement).into());
            }
            rasterize(scene, &self.receivers[i], r, width, &mut lookup, &mut jobs)?;
        }
        let mut values = Vec::with_capacity(jobs.len());
        // Keep SH readback scratch bounded independently of the page size.
        if let Some((seed, pages)) = benchmark {
            let mut state = *seed ^ (page as u64 + 1).wrapping_mul(0x9e3779b97f4a7c15);
            let raster = raster.expect("benchmark timer").elapsed();
            let mut ranges = Vec::new();
            let mut selected = Vec::new();
            let strata = jobs.len().div_ceil(64 * 512);
            for i in 0..strata {
                let lo = i * jobs.len() / strata;
                let hi = (i + 1) * jobs.len() / strata;
                let count = (hi - lo).min(512);
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let begin = lo + ((state >> 32) as usize) % (hi - lo - count + 1);
                ranges.push(begin..begin + count);
                selected.extend_from_slice(&jobs[begin..begin + count]);
            }
            // Match the full bake's native dispatch occupancy, even for disjoint strata.
            let mut sampled = Vec::with_capacity(selected.len());
            let start = std::time::Instant::now();
            for chunk in selected.chunks(8192) {
                bake.transport.trace(chunk, &mut sampled)?;
            }
            let elapsed = start.elapsed();
            let native = bake.transport.take_timing();
            values.resize(jobs.len(), [[0.; 3]; N]);
            let mut offset = 0;
            for r in ranges {
                let end = offset + r.len();
                values[r].copy_from_slice(&sampled[offset..end]);
                offset = end;
            }
            pages.pages.push(BakePageTiming {
                raster,
                receivers: jobs.len(),
                elapsed,
                gpu: (!native.missing_gpu).then_some(native.gpu),
                host_wait: native.wait,
                host_transfer: native.transfer,
                irradiance: sampled.into_iter().map(|v| v[0]).collect(),
            });
        } else {
            for chunk in jobs.chunks(8192) {
                bake.transport.trace(chunk, &mut values)?;
            }
        }
        Ok(PageSamples { lookup, values })
    }
}

fn rasterize(
    scene: &BakeScene,
    mesh: &StaticReceiver,
    region: SurfaceRegion,
    page_width: u32,
    lookup: &mut [usize],
    jobs: &mut Vec<SurfaceSample>,
) -> Result<(), BakeError> {
    let [w, h] = region.extent;
    let pitch = (w + 2) as usize;
    let mut coverage = vec![f32::INFINITY; pitch * (h + 2) as usize];
    let address = |x: u32, y: u32| {
        ((region.origin[1] + y - 1) * page_width + region.origin[0] + x - 1) as usize
    };
    let material = &scene.materials[mesh.material];
    for tri in mesh.indices.as_chunks::<3>().0 {
        let v = tri.map(|i| mesh.vertices[i as usize]);
        let geometric = cross(
            sub(v[1].position, v[0].position),
            sub(v[2].position, v[0].position),
        );
        if !dot(geometric, geometric).is_normal() {
            continue;
        }
        let p = v.map(|v| {
            [
                f64::from(v.lightmap_uv[0]) * f64::from(w) + 0.5,
                f64::from(v.lightmap_uv[1]) * f64::from(h) + 0.5,
            ]
        });
        let det = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
            (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
        };
        let area = det(p[0], p[1], p[2]);
        if area == 0. {
            return Err(BakeError(
                "nondegenerate receiver has collapsed lightmap UVs",
            ));
        }
        let bounds = |axis: usize, max: u32| {
            let coords = p.map(|xy| xy[axis]);
            let lo = coords.into_iter().fold(f64::INFINITY, f64::min).floor() as u32;
            let hi = coords.into_iter().fold(0., f64::max).ceil() as u32;
            lo..=hi.min(max + 1)
        };
        for y in bounds(1, h) {
            for x in bounds(0, w) {
                let center = [f64::from(x), f64::from(y)];
                let mut weights = [
                    det(p[1], p[2], center) / area,
                    det(p[2], p[0], center) / area,
                    det(p[0], p[1], center) / area,
                ];
                let mut distance = 0.;
                if weights.iter().any(|v| *v < 0.) {
                    distance = f64::INFINITY;
                    for i in 0..3 {
                        let j = (i + 1) % 3;
                        let [dx, dy] = [p[j][0] - p[i][0], p[j][1] - p[i][1]];
                        let t = (((center[0] - p[i][0]) * dx + (center[1] - p[i][1]) * dy)
                            / (dx * dx + dy * dy))
                            .clamp(0., 1.);
                        let d = (center[0] - p[i][0] - t * dx).powi(2)
                            + (center[1] - p[i][1] - t * dy).powi(2);
                        if d < distance {
                            distance = d;
                            weights = [0.; 3];
                            weights[i] = 1. - t;
                            weights[j] = t;
                        }
                    }
                    if distance > 0.5 {
                        continue;
                    }
                }
                let slot = y as usize * pitch + x as usize;
                if coverage[slot] <= distance as f32 {
                    continue;
                }
                let weights = weights.map(|a| a as f32);
                let interp = |a: [f32; 3]| dot(a, weights);
                if material.alpha_cutoff.is_some_and(|cutoff| {
                    let uv = std::array::from_fn(|k| interp(v.map(|v| v.material_uv[k])));
                    material.opacity
                        * material.base_texture.map_or(1., |i| {
                            scene.textures[i].sample(uv, material.samplers.base_color)[3]
                        })
                        < cutoff
                }) {
                    continue;
                }
                let normal = unit(std::array::from_fn(|k| interp(v.map(|v| v.normal[k]))))
                    .or_else(|_| unit(geometric))?;
                let sample = SurfaceSample {
                    position: std::array::from_fn(|k| interp(v.map(|v| v.position[k]))),
                    normal: if region.back {
                        normal.map(|a| -a)
                    } else {
                        normal
                    },
                };
                let target = &mut lookup[address(x, y)];
                if *target == usize::MAX {
                    *target = jobs.len();
                    jobs.push(sample);
                } else {
                    jobs[*target] = sample;
                }
                coverage[slot] = distance as f32;
            }
        }
    }
    // Copy only original surface samples within the two-texel filtering gutter.
    for y in 0..h + 2 {
        for x in 0..w + 2 {
            if coverage[y as usize * pitch + x as usize].is_finite() {
                continue;
            }
            let mut best = u32::MAX;
            for sy in y.saturating_sub(2)..=(y + 2).min(h + 1) {
                for sx in x.saturating_sub(2)..=(x + 2).min(w + 1) {
                    let d = x.abs_diff(sx).pow(2) + y.abs_diff(sy).pow(2);
                    if d < best && coverage[sy as usize * pitch + sx as usize].is_finite() {
                        lookup[address(x, y)] = lookup[address(sx, sy)];
                        best = d;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mask_uses_material_uv_and_both_sides_trace_opposite_hemispheres() {
        let triangles = vec![crate::Triangle {
            positions: [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            uv: [[0.; 2]; 3],
            material: 0,
        }];
        let mask = crate::Texture::new(2, 1, vec![[1., 1., 1., 0.], [1.; 4]]).unwrap();
        let scene = BakeScene::new(
            triangles,
            vec![crate::Material {
                base_texture: Some(0),
                alpha_cutoff: Some(0.5),
                ..Default::default()
            }],
            vec![mask],
        )
        .unwrap();
        let mut receiver = StaticReceiver {
            placement: 7,
            indices: vec![0, 1, 2],
            extent: [4, 4],
            material: 0,
            double_sided: true,
            vertices: [[0., 0.], [1., 0.], [0., 1.]]
                .map(|uv| LightmapVertex {
                    position: [uv[0], uv[1], 0.],
                    normal: [0., 0., 1.],
                    material_uv: [0.25, 0.5],
                    lightmap_uv: uv,
                })
                .to_vec(),
        };
        for (u, masked) in [(0.25, true), (0.75, false)] {
            for v in &mut receiver.vertices {
                v.material_uv[0] = u;
            }
            for back in [false, true] {
                let region = SurfaceRegion {
                    placement: 7,
                    page: 0,
                    back,
                    origin: [1, 1],
                    extent: [4, 4],
                };
                let mut pixels = [usize::MAX; 36];
                let mut samples = Vec::new();
                rasterize(&scene, &receiver, region, 6, &mut pixels, &mut samples).unwrap();
                assert_eq!(samples.is_empty(), masked);
                if !masked {
                    assert!(
                        samples
                            .iter()
                            .all(|s| s.normal[2] == if back { -1. } else { 1. })
                    );
                    assert_ne!(pixels[0], usize::MAX);
                    assert!(pixels.iter().filter(|p| **p != usize::MAX).count() > samples.len());
                }
            }
        }
    }

    #[test]
    fn selected_gpu_page_keeps_both_sides_and_skips_other_receivers() {
        let receiver = |placement: u32, double_sided: bool| StaticReceiver {
            placement,
            material: 0,
            extent: [6, 6],
            double_sided,
            vertices: [[0., 0.], [1., 0.], [0., 1.]]
                .map(|uv| LightmapVertex {
                    position: [uv[0], uv[1], 0.],
                    normal: [0., 0., 1.],
                    material_uv: uv,
                    lightmap_uv: uv,
                })
                .to_vec(),
            indices: vec![0, 1, 2],
        };
        let scene =
            BakeScene::new(Vec::new(), vec![crate::Material::default()], Vec::new()).unwrap();
        let prepare = || {
            scene
                .prepare_surface_bake(
                    crate::Lighting::uniform_sky([1.; 3]),
                    crate::BakeBackend::Cpu,
                    4,
                )
                .unwrap()
        };
        let mut receivers = [receiver(7, true), receiver(8, false)];
        // Collapsed UVs on a nondegenerate triangle fail rasterization. The
        // unselected chart must never be rasterized or traced.
        receivers[1]
            .vertices
            .iter_mut()
            .for_each(|v| v.lightmap_uv = [0.; 2]);
        let layout = SurfaceLayout::new(&receivers, 8, 1, None).unwrap();
        assert_eq!(layout.pages().len(), 3);
        assert_eq!(layout.gpu_page_count(), 2);
        let mut pages = Vec::new();
        layout
            .bake_gpu_pages(prepare(), &[0], |index, _, bytes| {
                assert_eq!(index, 0);
                pages.push(bytes);
                Ok(())
            })
            .unwrap();
        let words: Vec<_> = pages[0]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert_eq!(
            &words[..12],
            &[1., 1., 3., 0., 9., 8., 73., 8., 6., 6., 0., 0.]
        );
        assert!(words[12..].iter().any(|x| *x > 0.));
        assert!(
            layout
                .bake_gpu_pages(prepare(), &[1], |_, _, _| Ok(()))
                .is_err()
        );
        assert!(
            layout
                .bake_gpu_pages(prepare(), &[0, 0], |_, _, _| panic!(
                    "selection checked before writes"
                ))
                .is_err()
        );
    }

    #[test]
    fn scaled_instances_keep_uvs_and_budget_covers_both_sides() {
        let vertices = [[0., 0.], [1., 0.], [0., 1.]].map(|uv| LightmapVertex {
            position: [uv[0], uv[1], 0.],
            normal: [0., 0., 1.],
            lightmap_uv: uv,
            material_uv: [3., 4.],
        });
        let transform = sashiko_renderer::pbr::Transform::new([
            [2., 0., 0., 10.],
            [0., 2., 0., 0.],
            [0., 0., 2., 0.],
        ])
        .unwrap();
        let r = StaticReceiver::from_mesh(9, &vertices, &[0, 1, 2], [3, 3], 0, true, transform)
            .unwrap();
        assert_eq!(r.extent, [6, 6]);
        assert_eq!(r.vertices[1].position, [12., 0., 0.]);
        assert_eq!(r.vertices[1].lightmap_uv, vertices[1].lightmap_uv);
        let receivers = [r];
        let layout = SurfaceLayout::new(&receivers, 8, 1, Some(128)).unwrap();
        assert_eq!(layout.pages(), &[[8, 8], [8, 8]]);
        assert_eq!(
            layout
                .regions()
                .map(|r| (r.placement, r.page, r.back))
                .collect::<Vec<_>>(),
            [(9, 0, false), (9, 1, true)]
        );
        assert!(SurfaceLayout::new(&receivers, 8, 1, Some(127)).is_err());
    }
    #[test]
    fn adaptive_layout_preserves_gutters_sides_and_stable_addresses() {
        let mut receivers: Vec<_> = (0..31)
            .map(|i| StaticReceiver {
                placement: i,
                extent: [44 + (i * 7) % 90, 30 + (i * 11) % 65],
                double_sided: i % 3 != 0,
                material: (i % 4) as usize,
                indices: vec![0, 1, 2],
                vertices: vec![
                    LightmapVertex {
                        position: [0.; 3],
                        normal: [0., 1., 0.],
                        material_uv: [0.; 2],
                        lightmap_uv: [0.; 2]
                    };
                    3
                ],
            })
            .collect();
        let layout = SurfaceLayout::pack(&receivers, 3).unwrap();
        let mut regions: Vec<_> = layout.regions().collect();
        for (i, r) in regions.iter().enumerate() {
            let receiver = &receivers[r.placement as usize];
            assert_eq!(r.extent, receiver.extent);
            assert!(!r.back || receiver.double_sided);
            assert!(
                (0..2)
                    .all(|d| r.origin[d] >= 1
                        && r.origin[d] + r.extent[d] < layout.pages()[r.page][d])
            );
            for other in &regions[..i] {
                assert!(
                    r.page != other.page
                        || (0..2).any(|d| r.origin[d] + r.extent[d] + 1 < other.origin[d]
                            || other.origin[d] + other.extent[d] + 1 < r.origin[d])
                );
            }
        }
        assert_eq!(
            regions.len(),
            receivers.len() + receivers.iter().filter(|r| r.double_sided).count()
        );
        let pages = layout.pages().to_vec();
        let bytes = layout.resident_bytes();
        let texels = layout.texels();
        regions.sort_by_key(|r| (r.placement, r.back));
        receivers.reverse();
        let again = SurfaceLayout::new(&receivers, 2048, 3, Some(texels)).unwrap();
        let mut reordered: Vec<_> = again.regions().collect();
        reordered.sort_by_key(|r| (r.placement, r.back));
        assert_eq!(regions, reordered);
        assert_eq!(pages, again.pages());
        assert_eq!(bytes, again.resident_bytes());
    }
}

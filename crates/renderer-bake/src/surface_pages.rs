//! Offline allocation and rasterization of cooked lightmap UVs. A placement owns
//! its rectangle(s), even when several placements use identical geometry and UVs.
use crate::{
    BakeBackend, BakeError, BakeScene, LightingScenarios, SurfaceSample, Vec3, cross, dot, finite,
    sub, unit,
};
use std::{collections::HashSet, error::Error};

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
        transform: renderer::pbr::Transform,
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
}

impl<'a> SurfaceLayout<'a> {
    /// Choose standard page dimensions and cover every supplied placement.
    pub fn pack(receivers: &'a [StaticReceiver]) -> Result<Self, Box<dyn Error>> {
        Self::new(receivers, 2048, None)
    }

    /// Pack full-density rectangles with a one-texel border on every side.
    /// The optional budget counts allocated page texels per lighting scenario.
    pub fn new(
        receivers: &'a [StaticReceiver],
        page_side: u32,
        budget: Option<u64>,
    ) -> Result<Self, Box<dyn Error>> {
        if !(4..=2048).contains(&page_side) {
            return Err("invalid lightmap page size".into());
        }
        let mut order = Vec::new();
        let mut ids = HashSet::new();
        for (i, r) in receivers.iter().enumerate() {
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
            let [w, h] = r.extent.map(|v| v + 2);
            let (block, offset) = if !r.double_sided {
                ([w, h], None)
            } else if 2 * w <= page_side {
                ([2 * w, h], Some([w, 0]))
            } else if 2 * h <= page_side {
                ([w, 2 * h], Some([0, h]))
            } else {
                // Two dedicated pages keep the pair uploadable with one image binding.
                ([w, h], Some([0, 0]))
            };
            order.push((i, block, offset));
        }
        order.sort_by_key(|&(i, [w, h], _)| {
            (
                std::cmp::Reverse(h),
                std::cmp::Reverse(w),
                receivers[i].placement,
            )
        });
        let mut regions = Vec::new();
        let mut pages = Vec::new();
        let (mut x, mut y, mut row) = (0, 0, 0);
        let mut shelf = None;
        for (i, [w, h], offset) in order {
            let r = &receivers[i];
            let dedicated = offset == Some([0, 0]);
            let mut emit = |page, back, origin| {
                regions.push((
                    i,
                    SurfaceRegion {
                        placement: r.placement,
                        page,
                        back,
                        origin,
                        extent: r.extent,
                    },
                ))
            };
            if dedicated {
                for back in [false, true] {
                    emit(pages.len(), back, [1, 1]);
                    pages.push([w, h]);
                }
                continue;
            }
            if x + w > page_side {
                x = 0;
                y += row;
                row = 0;
            }
            if shelf.is_none() || y + h > page_side {
                shelf = Some(pages.len());
                pages.push([0, 0]);
                (x, y, row) = (0, 0, 0);
            }
            let page = shelf.expect("opened shelf");
            emit(page, false, [x + 1, y + 1]);
            if let Some([dx, dy]) = offset {
                emit(page, true, [x + dx + 1, y + dy + 1]);
            }
            x += w;
            row = row.max(h);
            pages[page][0] = pages[page][0].max(x);
            pages[page][1] = pages[page][1].max(y + h);
        }
        let layout = Self {
            receivers,
            regions,
            pages,
        };
        if let Some(limit) = budget
            && layout.texels() > limit
        {
            return Err(format!(
                "lightmaps require {} texels in {} pages; budget is {limit}",
                layout.texels(),
                layout.pages.len()
            )
            .into());
        }
        Ok(layout)
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

    /// Write a versioned surface-lighting asset followed by its probe volume.
    /// Returns the number of traced surface samples per page. Use a temporary
    /// output and publish it after success when replacing an existing asset.
    pub fn bake_to<const N: usize>(
        &self,
        scene: &BakeScene,
        scenarios: impl LightingScenarios<N>,
        settings: crate::ProbeSettings,
        rays_per_texel: u32,
        mut writer: impl std::io::Write,
    ) -> Result<Vec<usize>, Box<dyn Error>> {
        let lights = scene.lighting(scenarios)?;
        writer.write_all(renderer::pbr::SurfaceLightmaps::MAGIC)?;
        for n in [N, self.pages().len(), self.regions().count()] {
            writer.write_all(&u32::try_from(n)?.to_le_bytes())?;
        }
        for extent in self.pages() {
            for v in extent {
                writer.write_all(&v.to_le_bytes())?;
            }
        }
        for r in self.regions() {
            for v in [
                r.placement,
                u32::try_from(r.page)?,
                u32::from(r.back),
                r.origin[0],
                r.origin[1],
                r.extent[0],
                r.extent[1],
            ] {
                writer.write_all(&v.to_le_bytes())?;
            }
        }
        let mut counts = Vec::with_capacity(self.pages.len());
        self.bake(
            scene,
            lights,
            settings.backend,
            rays_per_texel,
            |_, data, samples| {
                for pixel in data {
                    for color in pixel {
                        for v in color {
                            writer.write_all(&v.to_le_bytes())?;
                        }
                    }
                }
                counts.push(samples);
                Ok(())
            },
        )?;
        scene.bake_probes(lights, settings)?.write(writer)?;
        Ok(counts)
    }
    /// Invoke the sink once per completed page. Padding copies its closest
    /// covered receiver, so no transport rays are spent on chart gutters.
    pub fn bake<const N: usize>(
        &self,
        scene: &BakeScene,
        scenarios: impl LightingScenarios<N>,
        backend: BakeBackend,
        rays: u32,
        mut write_page: impl FnMut(usize, &[[[f32; 3]; N]], usize) -> Result<(), Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        let lights = scene.lighting(scenarios)?;
        if rays == 0
            || self
                .receivers
                .iter()
                .any(|r| r.material >= scene.materials.len())
        {
            return Err("invalid lightmap material or ray count".into());
        }
        #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
        let mut gpu = if self.pages.is_empty() {
            None
        } else {
            crate::gpu::Transport::prepare(
                backend,
                scene,
                lights,
                rays,
                renderer::pbr::ProbeShOrder::L1,
            )?
        };
        #[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
        if !matches!(backend, BakeBackend::Auto | BakeBackend::Cpu) {
            return Err("requested GPU backend is not compiled".into());
        }
        for (page, &[width, height]) in self.pages.iter().enumerate() {
            let mut lookup = vec![usize::MAX; (width * height) as usize];
            let mut jobs = Vec::new();
            for &(i, r) in self.regions.iter().filter(|(_, r)| r.page == page) {
                rasterize(scene, &self.receivers[i], r, width, &mut lookup, &mut jobs)?;
            }
            let mut values = Vec::with_capacity(jobs.len());
            // Keep SH readback scratch bounded independently of the page size.
            for chunk in jobs.chunks(8192) {
                #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
                if let Some(gpu) = &mut gpu {
                    let raw = gpu.trace(chunk, 0.)?;
                    values.extend(
                        raw.chunks_exact(2 + 4 * N).map(|p| {
                            std::array::from_fn(|i| p[2 + 4 * i][..3].try_into().unwrap())
                        }),
                    );
                    continue;
                }
                values.extend(scene.integrate_surface(chunk, lights, rays)?);
            }
            let pixels: Vec<_> = lookup
                .into_iter()
                .map(|i| {
                    if i == usize::MAX {
                        [[0.; 3]; N]
                    } else {
                        values[i]
                    }
                })
                .collect();
            write_page(page, &pixels, jobs.len())?;
        }
        #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
        if let Some(gpu) = gpu {
            gpu.finish()?;
        }
        Ok(())
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
                        * material
                            .base_texture
                            .map_or(1., |i| scene.textures[i].sample(uv)[3])
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
    fn scaled_instances_keep_uvs_and_budget_covers_both_sides() {
        let vertices = [[0., 0.], [1., 0.], [0., 1.]].map(|uv| LightmapVertex {
            position: [uv[0], uv[1], 0.],
            normal: [0., 0., 1.],
            lightmap_uv: uv,
            material_uv: [3., 4.],
        });
        let transform =
            renderer::pbr::Transform::new([[2., 0., 0., 10.], [0., 2., 0., 0.], [0., 0., 2., 0.]])
                .unwrap();
        let r = StaticReceiver::from_mesh(9, &vertices, &[0, 1, 2], [3, 3], 0, true, transform)
            .unwrap();
        assert_eq!(r.extent, [6, 6]);
        assert_eq!(r.vertices[1].position, [12., 0., 0.]);
        assert_eq!(r.vertices[1].lightmap_uv, vertices[1].lightmap_uv);
        let receivers = [r];
        let layout = SurfaceLayout::new(&receivers, 8, Some(128)).unwrap();
        assert_eq!(layout.pages(), &[[8, 8], [8, 8]]);
        assert_eq!(
            layout
                .regions()
                .map(|r| (r.placement, r.page, r.back))
                .collect::<Vec<_>>(),
            [(9, 0, false), (9, 1, true)]
        );
        assert!(SurfaceLayout::new(&receivers, 8, Some(127)).is_err());
    }
}

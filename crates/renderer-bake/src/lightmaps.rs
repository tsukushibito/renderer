use crate::{
    BakeBackend, BakeError, BakeScene, Lighting, SurfaceSample, Vec3, dot, finite, sub, unit,
};
use renderer::pbr::LightmapChart;
use std::error::Error;

/// One static placement or LOD supplies its triangles in draw order. The three
/// shading normals and positions are in the same world space as the bake scene.
#[derive(Clone, Copy)]
pub struct ReceiverTriangle {
    pub positions: [Vec3; 3],
    pub normals: [Vec3; 3],
}
#[derive(Clone, Copy)]
pub struct LightmapSettings {
    pub texels_per_metre: f32,
    pub max_chart_side: u16,
    pub rays_per_texel: u32,
    pub backend: BakeBackend,
}
impl Default for LightmapSettings {
    fn default() -> Self {
        Self {
            texels_per_metre: 4.,
            max_chart_side: 32,
            rays_per_texel: 128,
            backend: BakeBackend::Auto,
        }
    }
}
impl ReceiverTriangle {
    pub fn chart_extent(&self, settings: LightmapSettings) -> Result<[u16; 2], BakeError> {
        if !settings.texels_per_metre.is_normal()
            || settings.texels_per_metre <= 0.
            || !(2..=256).contains(&settings.max_chart_side)
            || self.positions.iter().any(|p| !finite(*p))
        {
            return Err(BakeError("invalid surface chart bounds or texel density"));
        }
        Ok([1, 2].map(|j| {
            let edge = sub(self.positions[j], self.positions[0]);
            (1. + (dot(edge, edge).sqrt() * settings.texels_per_metre).ceil())
                .clamp(2., f32::from(settings.max_chart_side)) as u16
        }))
    }
}
impl BakeScene {
    /// Generate independent bilinear charts. Edge extension projects unused
    /// texels onto the nearest triangle edge, so filtering never crosses charts.
    pub fn bake_lightmaps(
        &self,
        triangles: &[ReceiverTriangle],
        scenarios: [Lighting; 2],
        settings: LightmapSettings,
    ) -> Result<Vec<LightmapChart>, Box<dyn Error>> {
        if settings.rays_per_texel == 0 {
            return Err(BakeError("zero surface rays").into());
        }
        let [a, b] = scenarios;
        let lights = [a.normalized()?, b.normalized()?];
        let mut jobs = Vec::new();
        let mut extents = Vec::with_capacity(triangles.len());
        for tri in triangles {
            let extent = tri.chart_extent(settings)?;
            let [w, h] = extent;
            if jobs.len() + usize::from(w) * usize::from(h) > 256 * 16384 / 2 {
                return Err(BakeError("lightmap atlas exceeds capacity").into());
            }
            let [a, b, c] = tri.normals;
            let normals = [unit(a)?, unit(b)?, unit(c)?];
            for y in 0..h {
                for x in 0..w {
                    let u = f32::from(x) / f32::from(w - 1);
                    let v = f32::from(y) / f32::from(h - 1);
                    let excess = (u + v - 1.).max(0.) * 0.5;
                    let weights = [(1. - u - v).max(0.), u - excess, v - excess];
                    let interp = |values: [Vec3; 3]| {
                        std::array::from_fn(|i| (0..3).map(|j| weights[j] * values[j][i]).sum())
                    };
                    jobs.push(SurfaceSample {
                        position: interp(tri.positions),
                        normal: unit(interp(normals))?,
                    });
                }
            }
            extents.push(extent);
        }
        let irradiance: Vec<_> = if let Some(gpu) =
            settings
                .backend
                .integrate(self, &jobs, lights, settings.rays_per_texel, 0.)?
        {
            gpu.as_chunks::<10>()
                .0
                .iter()
                .map(|p| [p[2][..3].try_into().unwrap(), p[6][..3].try_into().unwrap()])
                .collect()
        } else {
            self.bake_irradiance(&jobs, lights[0], settings.rays_per_texel)?
                .into_iter()
                .zip(self.bake_irradiance(&jobs, lights[1], settings.rays_per_texel)?)
                .map(|(a, b)| [a, b])
                .collect()
        };
        let mut samples = irradiance.into_iter();
        Ok(extents
            .into_iter()
            .map(|extent| LightmapChart {
                extent,
                irradiance: samples
                    .by_ref()
                    .take(usize::from(extent[0]) * usize::from(extent[1]))
                    .collect(),
            })
            .collect())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_chart_extents_and_two_linear_scenarios() {
        let scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        let tri = ReceiverTriangle {
            positions: [[0.; 3], [1., 0., 0.], [0., 0., 2.]],
            normals: [[0., 1., 0.]; 3],
        };
        let light = Lighting {
            sky: [1.; 3],
            sun: [10.; 3],
            direction_to_sun: [0., 1., 0.],
        };
        let scenarios = [
            light,
            Lighting {
                sky: [0.; 3],
                ..light
            },
        ];
        let maps = scene
            .bake_lightmaps(
                &[tri],
                scenarios,
                LightmapSettings {
                    backend: BakeBackend::Cpu,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(maps[0].extent, [5, 9]);
        assert!(
            maps[0]
                .irradiance
                .iter()
                .all(|p| (p[0][0] - std::f32::consts::PI).abs() < 1e-5 && p[1] == [0.; 3])
        );
        let atlas = scene
            .bake_probes(
                scenarios,
                crate::ProbeSettings {
                    backend: BakeBackend::Cpu,
                    ..Default::default()
                },
            )
            .unwrap()
            .with_lightmaps(&maps)
            .unwrap();
        let mut file = vec![];
        atlas.write(&mut file).unwrap();
        assert_eq!(
            renderer::pbr::BakedLighting::from_bytes(&file)
                .unwrap()
                .rgba32f(),
            atlas.rgba32f()
        );
    }
}

//! Offline diffuse and specular lighting in linear RGB. Coordinates are world space in metres.
#![deny(unsafe_code)]

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
mod gpu;
#[cfg(feature = "vulkan")]
mod ray_query;
#[cfg(feature = "film")]
pub use ray_query::film::{FilmIntersection, FilmTracer};

/// Automatic selection tries available GPU backends and falls back to CPU at
/// device preparation. Explicit selections propagate setup and execution errors.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BakeBackend {
    #[default]
    Auto,
    Cpu,
    Vulkan,
    /// Require hardware traversal for static surfaces.
    VulkanRayQuery,
    /// Force portable compute traversal on Vulkan.
    VulkanSoftware,
    D3D12,
}

mod execution;
pub use execution::{BakeExecution, BakeMethod, SurfaceBake};
pub use probes::ProbeBake;

mod bvh;
mod lightmaps;
mod surface_pages;
pub use surface_pages::{
    BakeBenchmark, BakePageTiming, LightmapVertex, StaticReceiver, SurfaceLayout, SurfaceRegion,
};
mod probes;
mod reflection_placement;
mod reflection_quality;
pub use reflection_quality::{PlacementDecision, PlacementQuality, QualityMetrics};
mod reflection_reference;
mod reflections;

pub use probes::ProbeSettings;
pub use reflection_placement::{ReflectionPlacementMethod, ReflectionPlacementSettings};
pub use reflection_reference::{ReflectionReceiver, ReflectionReference};
pub use sashiko_renderer::pbr::{PointLight, ProbeShOrder, RectLight, ReflectionProbeDesc, Sun};
mod transport;

use bvh::Bvh;
use std::fmt;
pub use transport::{Lighting, LightingScenarios, SkyMap, SurfaceSample};

pub type Vec3 = [f32; 3];

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    std::array::from_fn(|i| a[i] - b[i])
}
fn dot(a: Vec3, b: Vec3) -> f32 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}
fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn unit(v: Vec3) -> Result<Vec3, BakeError> {
    let len = dot(v, v).sqrt();
    if !len.is_normal() {
        return Err(BakeError("expected a finite nonzero direction"));
    }
    Ok(v.map(|x| x / len))
}
fn finite(v: Vec3) -> bool {
    v.into_iter().all(f32::is_finite)
}
fn nonnegative(v: Vec3) -> bool {
    finite(v) && v.into_iter().all(|x| x >= 0.)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BakeError(&'static str);
impl fmt::Display for BakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for BakeError {}

use sashiko_renderer::core::program::{AddressMode, FilterMode, SamplerDescriptor};
use sashiko_renderer::pbr::MaterialTextures;

/// Linear RGBA base level for offline transport, with a top-left origin.
/// All material lookups evaluate mip zero at the specified magnification filter.
pub struct Texture {
    width: usize,
    height: usize,
    pixels: Vec<[f32; 4]>,
}
impl Texture {
    pub fn new(width: usize, height: usize, pixels: Vec<[f32; 4]>) -> Result<Self, BakeError> {
        if width == 0
            || height == 0
            || width.checked_mul(height) != Some(pixels.len())
            || pixels.iter().flatten().any(|x| !x.is_finite() || *x < 0.)
        {
            return Err(BakeError("invalid bake texture"));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }
    fn sample(&self, uv: [f32; 2], sampler: SamplerDescriptor) -> [f32; 4] {
        // Reduce before multiplying by the extent, including for very large finite UVs.
        let coord = |uv: f32, size: usize, mode| {
            let normalized = match mode {
                AddressMode::Repeat => uv.rem_euclid(1.),
                AddressMode::MirroredRepeat => uv.rem_euclid(2.),
                AddressMode::ClampToEdge => uv.clamp(0., 1.),
            };
            normalized * size as f32
        };
        let address = |i: isize, size: usize, mode| {
            let n = size as isize;
            (match mode {
                AddressMode::ClampToEdge => i.clamp(0, n - 1),
                AddressMode::Repeat => i.rem_euclid(n),
                AddressMode::MirroredRepeat => {
                    let k = i.rem_euclid(2 * n);
                    k.min(2 * n - 1 - k)
                }
            }) as usize
        };
        let x = coord(uv[0], self.width, sampler.wrap_u);
        let y = coord(uv[1], self.height, sampler.wrap_v);
        let pixel = |x, y| {
            self.pixels[address(y, self.height, sampler.wrap_v) * self.width
                + address(x, self.width, sampler.wrap_u)]
        };
        if sampler.mag_filter == FilterMode::Nearest {
            return pixel(x.floor() as isize, y.floor() as isize);
        }
        let (x, y) = (x - 0.5, y - 0.5);
        let at = |dx, dy| pixel(x.floor() as isize + dx, y.floor() as isize + dy);
        let [a, b, c, d] = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)];
        let (u, v) = (x - x.floor(), y - y.floor());
        std::array::from_fn(|i| {
            (a[i] * (1. - u) + b[i] * u) * (1. - v) + (c[i] * (1. - u) + d[i] * u) * v
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub diffuse: Vec3,
    /// Linear Rec.709 emission for a white emissive texel, with luminance Y in cd/m².
    pub emissive_nits: Vec3,
    /// The image may be shared across slots and materials with different sampling.
    pub samplers: MaterialTextures<SamplerDescriptor>,
    pub base_texture: Option<usize>,
    pub alpha_cutoff: Option<f32>,
    pub opacity: f32,
    pub metallic: f32,
    /// Linear ORM texture. The blue channel modulates metallic.
    pub orm_texture: Option<usize>,
    pub emission_texture: Option<usize>,
}
impl Default for Material {
    fn default() -> Self {
        Self {
            diffuse: [1.; 3],
            emissive_nits: [0.; 3],
            samplers: MaterialTextures::default(),
            base_texture: None,
            alpha_cutoff: None,
            opacity: 1.,
            metallic: 0.,
            orm_texture: None,
            emission_texture: None,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Triangle {
    pub positions: [Vec3; 3],
    pub uv: [[f32; 2]; 3],
    pub material: usize,
}

/// Immutable geometry shared by all lighting scenarios.
pub struct BakeScene {
    triangles: Vec<Triangle>,
    materials: Vec<Material>,
    textures: Vec<Texture>,
    bvh: Bvh,
    rect_lights: Vec<RectLight>,
    point_lights: Vec<PointLight>,
}

impl BakeScene {
    /// Replace static point emitters contributing bounced light in all scenarios.
    pub fn set_point_lights(&mut self, lights: Vec<PointLight>) {
        self.point_lights = lights;
    }

    /// Replace static rectangular emitters for all baked scenarios. Direct light
    /// at the final receiver is evaluated by the runtime from the same descriptors.
    pub fn set_rect_lights(&mut self, lights: Vec<RectLight>) {
        self.rect_lights = lights;
    }

    /// Add a top-origin latitude-longitude sky at the bake boundary.
    pub fn add_sky_map(&mut self, texture: Texture, yaw_radians: f32) -> Result<SkyMap, BakeError> {
        if !yaw_radians.is_finite() {
            return Err(BakeError("invalid sky yaw"));
        }
        let map = SkyMap {
            texture: self.textures.len(),
            yaw_radians,
        };
        self.textures.push(texture);
        Ok(map)
    }

    pub fn new(
        mut triangles: Vec<Triangle>,
        materials: Vec<Material>,
        textures: Vec<Texture>,
    ) -> Result<Self, BakeError> {
        for m in &materials {
            if !nonnegative(m.diffuse)
                || m.diffuse.into_iter().any(|x| x > 1.)
                || !nonnegative(m.emissive_nits)
                || [m.base_texture, m.orm_texture, m.emission_texture]
                    .into_iter()
                    .flatten()
                    .any(|i| i >= textures.len())
                || ![m.opacity, m.metallic]
                    .iter()
                    .all(|v| (0.0..=1.0).contains(v))
                || m.alpha_cutoff.is_some_and(|x| !x.is_finite() || x < 0.)
            {
                return Err(BakeError("invalid diffuse bake material"));
            }
        }
        if triangles.iter().any(|t| {
            t.material >= materials.len()
                || t.positions.iter().any(|v| !finite(*v))
                || !t.uv.iter().flatten().all(|x| x.is_finite())
        }) {
            return Err(BakeError("invalid bake triangle"));
        }
        triangles.retain(|t| dot(t.normal(), t.normal()).is_normal());
        let bvh = Bvh::new(&mut triangles)?;
        Ok(Self {
            triangles,
            materials,
            textures,
            bvh,
            rect_lights: Vec::new(),
            point_lights: Vec::new(),
        })
    }
}
impl Triangle {
    fn normal(&self) -> Vec3 {
        cross(
            sub(self.positions[1], self.positions[0]),
            sub(self.positions[2], self.positions[0]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bake_texture_addresses_each_axis_at_mip_zero() {
        // Rows encode column + 10 * row. At this UV, all three address modes
        // select different texels along both axes.
        let pixels = (0..16)
            .map(|i| [(i % 4 + 10 * (i / 4)) as f32; 4])
            .collect();
        let texture = Texture::new(4, 4, pixels).unwrap();
        let modes = [
            AddressMode::Repeat,
            AddressMode::ClampToEdge,
            AddressMode::MirroredRepeat,
        ];
        for (u, x) in modes.into_iter().zip([2, 0, 1]) {
            for (v, y) in modes.into_iter().zip([1, 3, 2]) {
                let sampler = SamplerDescriptor {
                    wrap_u: u,
                    wrap_v: v,
                    mag_filter: FilterMode::Nearest,
                    ..Default::default()
                };
                assert_eq!(
                    texture.sample([-0.375, 1.375], sampler),
                    [(x + 10 * y) as f32; 4]
                );
            }
        }
        for (wrap, expected) in [
            (AddressMode::Repeat, 1.5),
            (AddressMode::ClampToEdge, 0.),
            (AddressMode::MirroredRepeat, 0.),
        ] {
            let s = SamplerDescriptor {
                wrap_u: wrap,
                wrap_v: AddressMode::ClampToEdge,
                min_filter: FilterMode::Nearest,
                ..Default::default()
            };
            assert_eq!(texture.sample([0., 0.], s), [expected; 4]);
        }
    }
}

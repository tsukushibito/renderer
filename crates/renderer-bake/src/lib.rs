//! Offline diffuse lighting in linear RGB. Coordinates are world space in metres.
#![deny(unsafe_code)]

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
mod gpu;

/// Automatic selection tries available GPU backends and falls back to CPU at
/// device preparation. Explicit selections propagate setup and execution errors.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BakeBackend {
    #[default]
    Auto,
    Cpu,
    Vulkan,
    D3D12,
}

impl BakeBackend {
    fn integrate(
        self,
        scene: &BakeScene,
        jobs: &[SurfaceSample],
        lights: [Lighting; 2],
        samples: u32,
        limit: f32,
    ) -> Result<Option<Vec<[f32; 4]>>, Box<dyn std::error::Error>> {
        #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
        if self != Self::Cpu {
            return gpu::integrate(self, scene, jobs, lights, samples, limit);
        }
        #[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
        let _ = (scene, jobs, lights, samples, limit);
        if matches!(self, Self::Auto | Self::Cpu) {
            Ok(None)
        } else {
            Err("requested GPU backend is not compiled".into())
        }
    }
}

mod bvh;
mod lightmaps;
mod probes;
pub use lightmaps::{LightmapSettings, ReceiverTriangle};
pub use probes::ProbeSettings;
mod transport;

use bvh::Bvh;
use std::fmt;
pub use transport::{Lighting, SurfaceSample};

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

/// Linear RGBA, top-left origin, repeating UVs and bilinear sampling.
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
    fn sample(&self, uv: [f32; 2]) -> [f32; 4] {
        let x = uv[0].rem_euclid(1.) * self.width as f32 - 0.5;
        let y = uv[1].rem_euclid(1.) * self.height as f32 - 0.5;
        let (ix, iy) = (x.floor() as isize, y.floor() as isize);
        let at = |dx, dy| {
            self.pixels[(iy + dy as isize).rem_euclid(self.height as isize) as usize * self.width
                + (ix + dx as isize).rem_euclid(self.width as isize) as usize]
        };
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
    pub emission: Vec3,
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
            emission: [0.; 3],
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
}

impl BakeScene {
    pub fn new(
        mut triangles: Vec<Triangle>,
        materials: Vec<Material>,
        textures: Vec<Texture>,
    ) -> Result<Self, BakeError> {
        for m in &materials {
            if !nonnegative(m.diffuse)
                || m.diffuse.into_iter().any(|x| x > 1.)
                || !nonnegative(m.emission)
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

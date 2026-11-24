use crate::{BakeScene, SurfaceSample};
use std::error::Error;
impl BakeScene {
    pub(crate) fn integrate_surface<const N: usize>(
        &self,
        jobs: &[SurfaceSample],
        lights: [crate::Lighting; N],
        rays: u32,
    ) -> Result<Vec<[[f32; 3]; N]>, Box<dyn Error>> {
        let mut output = vec![[[0.; 3]; N]; jobs.len()];
        for (i, light) in lights.into_iter().enumerate() {
            for (texel, rgb) in output
                .iter_mut()
                .zip(self.bake_irradiance(jobs, light, rays)?)
            {
                texel[i] = rgb;
            }
        }
        Ok(output)
    }
}

#[derive(Default, Clone, Copy)]
pub(crate) struct DispatchTiming {
    pub gpu: std::time::Duration,
    pub wait: std::time::Duration,
    pub transfer: std::time::Duration,
    pub missing_gpu: bool,
}
pub(crate) enum SurfaceTransport<'a, const N: usize> {
    Cpu(&'a BakeScene, [crate::Lighting; N], u32),
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    Compute(Box<crate::gpu::Transport<N>>),
    #[cfg(feature = "vulkan")]
    RayQuery(Box<crate::ray_query::Transport<N>>),
}
impl<'a, const N: usize> SurfaceTransport<'a, N> {
    pub fn new(
        scene: &'a BakeScene,
        lights: [crate::Lighting; N],
        backend: crate::BakeBackend,
        rays: u32,
    ) -> Result<Self, Box<dyn Error>> {
        use crate::BakeBackend;
        #[cfg(feature = "vulkan")]
        if matches!(
            backend,
            BakeBackend::Vulkan | BakeBackend::Auto | BakeBackend::VulkanRayQuery
        ) {
            match crate::ray_query::Transport::prepare(scene, lights, rays) {
                Ok(Some(hw)) => return Ok(Self::RayQuery(Box::new(hw))),
                Ok(None) if backend == BakeBackend::VulkanRayQuery => {
                    return Err("Vulkan ray queries unavailable".into());
                }
                Err(e) if backend == BakeBackend::Auto => {
                    eprintln!("Ray query initialization: {e}")
                }
                Err(e) => return Err(e),
                Ok(None) => {}
            }
        }
        #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
        if let Some(gpu) = crate::gpu::Transport::prepare(backend, scene, lights, rays, None)? {
            return Ok(Self::Compute(Box::new(gpu)));
        }
        if !matches!(backend, BakeBackend::Auto | BakeBackend::Cpu) {
            return Err("requested GPU backend unavailable".into());
        }
        Ok(Self::Cpu(scene, lights, rays))
    }
    pub fn trace(
        &mut self,
        jobs: &[SurfaceSample],
        values: &mut Vec<[crate::Vec3; N]>,
    ) -> Result<(), Box<dyn Error>> {
        match self {
            Self::Cpu(scene, lights, rays) => {
                values.extend(scene.integrate_surface(jobs, *lights, *rays)?)
            }
            #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
            Self::Compute(gpu) => values.extend(
                gpu.trace(jobs, 0.)?
                    .as_chunks::<N>()
                    .0
                    .iter()
                    .map(|p| std::array::from_fn(|i| p[i][..3].try_into().unwrap())),
            ),
            #[cfg(feature = "vulkan")]
            Self::RayQuery(hw) => hw.trace(jobs, values)?,
        }
        Ok(())
    }
    pub fn enable_timing(&mut self) -> Result<(), Box<dyn Error>> {
        match self {
            Self::Cpu(..) => Ok(()),
            #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
            Self::Compute(g) => g.enable_timing(),
            #[cfg(feature = "vulkan")]
            Self::RayQuery(g) => g.enable_timing(),
        }
    }
    pub fn take_timing(&mut self) -> DispatchTiming {
        match self {
            Self::Cpu(..) => DispatchTiming {
                missing_gpu: true,
                ..Default::default()
            },
            #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
            Self::Compute(g) => g.take_timing(),
            #[cfg(feature = "vulkan")]
            Self::RayQuery(g) => g.take_timing(),
        }
    }
    pub fn finish(self) -> Result<(), Box<dyn Error>> {
        match self {
            Self::Cpu(..) => Ok(()),
            #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
            Self::Compute(gpu) => gpu.finish(),
            #[cfg(feature = "vulkan")]
            Self::RayQuery(hw) => hw.finish(),
        }
    }
}

impl BakeScene {
    /// Evaluate specified world-space receivers with the production surface transport.
    pub fn sample_surface_irradiance(
        &self,
        points: &[SurfaceSample],
        light: crate::Lighting,
        settings: crate::ProbeSettings,
    ) -> Result<Vec<crate::Vec3>, Box<dyn Error>> {
        let lights = self.lighting([light])?;
        if settings.rays_per_probe == 0 {
            return Err("zero transport samples".into());
        }
        let points = points
            .iter()
            .map(|p| {
                if !crate::finite(p.position) {
                    return Err(crate::BakeError("invalid receiver position"));
                }
                Ok(SurfaceSample {
                    position: p.position,
                    normal: crate::unit(p.normal)?,
                })
            })
            .collect::<Result<Vec<_>, crate::BakeError>>()?;
        let mut transport =
            SurfaceTransport::new(self, lights, settings.backend, settings.rays_per_probe)?;
        let mut output = Vec::with_capacity(points.len());
        transport.trace(&points, &mut output)?;
        transport.finish()?;
        Ok(output.into_iter().map(|[rgb]| rgb).collect())
    }
}

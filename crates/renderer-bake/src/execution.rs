//! Concrete offline execution selected before any receivers are integrated.
use crate::{BakeBackend, BakeScene, LightingScenarios, lightmaps::SurfaceTransport};
use std::error::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BakeMethod {
    Cpu,
    VulkanRayQuery,
    VulkanCompute,
    D3D12Compute,
}

/// Facts from the device and shader that will run this stage. A subsequent
/// execution error fails the stage; the selected implementation stays fixed.
#[derive(Clone)]
pub struct BakeExecution {
    pub method: BakeMethod,
    pub device: String,
    pub driver: String,
    pub shader: Option<&'static [u8]>,
}

impl BakeExecution {
    pub(crate) fn cpu() -> Self {
        Self {
            method: BakeMethod::Cpu,
            device: std::env::consts::ARCH.into(),
            driver: String::new(),
            shader: None,
        }
    }

    #[cfg(feature = "vulkan")]
    pub(crate) fn vulkan(
        runtime: &renderer::core::vulkan::VulkanRuntime,
        method: BakeMethod,
        shader: &'static [u8],
    ) -> Self {
        let info = runtime.device_info();
        Self {
            method,
            device: format!(
                "{:04x}:{:04x} {}",
                info.vendor_id, info.device_id, info.name
            ),
            driver: format!(
                "{}:{}:{}:{}",
                info.driver_id_raw,
                info.driver_version_raw,
                info.api_version_raw,
                info.driver_info.as_deref().unwrap_or("")
            ),
            shader: Some(shader),
        }
    }
}

/// Holds the selected transport and source scene throughout a surface bake.
pub struct SurfaceBake<'a, const N: usize> {
    pub(crate) transport: SurfaceTransport<'a, N>,
    scene: &'a BakeScene,
    execution: BakeExecution,
}

impl BakeScene {
    pub fn prepare_surface_bake<const N: usize>(
        &self,
        scenarios: impl LightingScenarios<N>,
        backend: BakeBackend,
        rays: u32,
    ) -> Result<SurfaceBake<'_, N>, Box<dyn Error>> {
        let lights = self.lighting(scenarios)?;
        if rays == 0 {
            return Err("zero surface samples".into());
        }
        let transport = SurfaceTransport::new(self, lights, backend, rays)?;
        let execution = match &transport {
            SurfaceTransport::Cpu(..) => BakeExecution::cpu(),
            #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
            SurfaceTransport::Compute(gpu) => gpu.execution.clone(),
            #[cfg(feature = "vulkan")]
            SurfaceTransport::RayQuery(gpu) => gpu.execution.clone(),
        };
        Ok(SurfaceBake {
            transport,
            scene: self,
            execution,
        })
    }
}
impl<'a, const N: usize> SurfaceBake<'a, N> {
    pub fn execution(&self) -> &BakeExecution {
        &self.execution
    }
    pub fn scene(&self) -> &'a BakeScene {
        self.scene
    }
    pub fn finish(self) -> Result<(), Box<dyn Error>> {
        self.transport.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Lighting, SurfaceSample};

    #[test]
    fn fixed_cpu_surface_repeats_before_publication() -> Result<(), Box<dyn Error>> {
        let scene = BakeScene::new(Vec::new(), Vec::new(), Vec::new())?;
        let run = || -> Result<_, Box<dyn Error>> {
            let light = Lighting::uniform_sky([2., 3., 4.]);
            let mut bake = scene.prepare_surface_bake(light, BakeBackend::Cpu, 8)?;
            assert_eq!(bake.execution().method, BakeMethod::Cpu);
            assert!(bake.execution().shader.is_none());
            let mut out = Vec::new();
            bake.transport.trace(
                &[SurfaceSample {
                    position: [0.; 3],
                    normal: [0., 1., 0.],
                }],
                &mut out,
            )?;
            bake.finish()?;
            Ok(out[0][0].map(f32::to_bits))
        };
        assert_eq!(run()?, run()?);
        Ok(())
    }
}

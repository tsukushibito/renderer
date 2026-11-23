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

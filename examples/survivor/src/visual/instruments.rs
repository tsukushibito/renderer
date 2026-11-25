use super::geometry::Geometry;
use sashiko_renderer::pipeline::PipelineResult;

/// All instruments share the hall's material batches. Grouping by surface keeps
/// the renderer cost independent of the number of vial caps and grille slats.
pub(super) struct Equipment<'a> {
    pub enamel: &'a mut Geometry,
    pub alloy: &'a mut Geometry,
    pub dark: &'a mut Geometry,
    pub teal: &'a mut Geometry,
    pub amber: &'a mut Geometry,
}

impl Equipment<'_> {
    pub fn microscope(&mut self, x: f32, z: f32) -> PipelineResult<()> {
        self.enamel.slab([x, 1.09, z], [0.62, 0.14, 0.63])?;
        self.enamel.tube(
            [x - 0.18, 1.11, z - 0.16],
            [x - 0.24, 1.77, z - 0.09],
            0.12,
            0.09,
        )?;
        self.enamel
            .tube([x - 0.24, 1.77, z - 0.09], [x + 0.06, 1.88, z], 0.09, 0.095)?;
        self.dark.slab([x + 0.03, 1.46, z], [0.47, 0.055, 0.34])?;
        self.alloy.tube([x, 1.22, z], [x, 1.37, z], 0.12, 0.085)?;
        self.alloy
            .tube([x + 0.09, 1.71, z], [x + 0.09, 1.84, z], 0.095, 0.12)?;
        for dx in [-0.05, 0.05] {
            self.dark.tube(
                [x + 0.09 + dx, 1.58, z],
                [x + 0.09 + dx, 1.72, z],
                0.026,
                0.036,
            )?;
            self.enamel.tube(
                [x + 0.06 + dx, 1.84, z],
                [x + 0.06 + 1.4 * dx, 2.05, z + 0.14],
                0.058,
                0.044,
            )?;
            self.dark.tube(
                [x + 0.06 + 1.4 * dx, 2.05, z + 0.14],
                [x + 0.06 + 1.4 * dx, 2.12, z + 0.19],
                0.052,
                0.054,
            )?;
        }
        self.dark.tube(
            [x - 0.40, 1.56, z - 0.08],
            [x - 0.23, 1.56, z - 0.08],
            0.09,
            0.09,
        )?;
        Ok(())
    }
    pub fn rack(&mut self, x: f32, z: f32) -> PipelineResult<()> {
        self.alloy.slab([x, 1.08, z], [0.80, 0.09, 0.46])?;
        self.enamel.slab([x, 1.28, z], [0.80, 0.07, 0.46])?;
        for row in [-1., 1.] {
            for i in 0..4 {
                let vx = x - 0.29 + i as f32 * 0.19;
                let vz = z + row * 0.105;
                self.teal
                    .tube([vx, 1.13, vz], [vx, 1.54, vz], 0.060, 0.063)?;
                self.amber
                    .tube([vx, 1.54, vz], [vx, 1.61, vz], 0.068, 0.068)?;
            }
        }
        Ok(())
    }
    pub fn tank(&mut self, x: f32, z: f32) -> PipelineResult<()> {
        let shaft = |h| [x, h, z];
        self.alloy.tube(shaft(1.015), shaft(1.13), 0.48, 0.50)?;
        self.enamel.tube(shaft(1.13), shaft(1.23), 0.46, 0.43)?;
        self.teal.tube(shaft(1.23), shaft(2.05), 0.425, 0.425)?;
        self.alloy.tube(shaft(2.05), shaft(2.15), 0.46, 0.49)?;
        self.enamel.tube(shaft(2.15), shaft(2.33), 0.49, 0.18)?;
        self.dark.tube(shaft(2.33), shaft(2.42), 0.075, 0.075)?;
        self.alloy
            .tube(shaft(2.43), [x + 0.48, 2.43, z], 0.050, 0.050)?;
        self.alloy
            .tube([x + 0.48, 2.43, z], [x + 0.48, 1.04, z], 0.05, 0.05)?;
        for i in 0..4 {
            let (s, c) = (std::f32::consts::FRAC_PI_2 * i as f32 + 0.5).sin_cos();
            let xx = x + s * 0.455;
            let zz = z + c * 0.455;
            self.alloy
                .tube([xx, 1.13, zz], [xx, 2.14, zz], 0.028, 0.028)?;
        }
        Ok(())
    }
    pub fn display(&mut self, x: f32, z: f32) -> PipelineResult<()> {
        self.dark.slab([x, 1.035, z], [0.55, 0.05, 0.33])?;
        self.alloy
            .tube([x, 1.05, z], [x, 1.37, z - 0.05], 0.045, 0.055)?;
        self.dark.slab([x, 1.63, z], [0.78, 0.54, 0.08])?;
        self.teal.slab([x, 1.64, z + 0.041], [0.70, 0.42, 0.01])?;
        self.enamel
            .slab([x - 0.12, 1.72, z + 0.05], [0.32, 0.032, 0.01])?;
        for k in 0..6 {
            let h = 0.05 + (k as f32 * 1.8).sin().abs() * 0.12;
            self.dark.slab(
                [x - 0.26 + k as f32 * 0.09, 1.49 + h / 2., z + 0.06],
                [0.06, h, 0.01],
            )?;
        }
        self.dark.slab([x, 1.04, z + 0.34], [0.72, 0.05, 0.21])?;
        Ok(())
    }
}

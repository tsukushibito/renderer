//! Orthographic framing and extraction of objects positioned on the XZ plane.
use renderer::{
    pbr::{Projection, Transform, ViewInput},
    pipeline::{CameraView, PipelineResult, RenderObject, ViewId},
};

/// Camera looks toward negative Z from above the ground. Height and distance are metres.
#[derive(Clone, Copy, Debug)]
pub struct GroundCamera {
    pub height: f32,
    pub distance: f32,
    pub pitch_radians: f32,
    pub depth_range: [f32; 2],
}

impl GroundCamera {
    pub fn view(
        self,
        id: ViewId,
        target: [f32; 2],
        extent: [u32; 2],
    ) -> PipelineResult<CameraView> {
        let [width, height] = extent;
        if width == 0 || height == 0 {
            return Err("camera requires a nonempty viewport".into());
        }
        let [x, z] = target;
        let half_height = self.height * 0.5;
        if !(half_height > 0.
            && half_height.is_finite()
            && self.pitch_radians > 0.
            && self.pitch_radians <= std::f32::consts::FRAC_PI_2
            && self.distance > 0.)
        {
            return Err("invalid orthographic ground camera".into());
        }
        let half_width = half_height * width as f32 / height as f32;
        let distance = self.distance;
        let (s, c) = self.pitch_radians.sin_cos();
        let eye = [x, distance * s, z + distance * c];
        let [near, far] = self.depth_range;
        let depth = far - near;
        let matrix = [
            [1. / half_width, 0., 0., 0.],
            [0., -c / half_height, -s / depth, 0.],
            [0., s / half_height, -c / depth, 0.],
            [
                -x / half_width,
                -s * z / half_height,
                (distance + c * z - near) / depth,
                1.,
            ],
        ];
        Ok(CameraView {
            id,
            extent,
            camera: ViewInput::new(
                matrix,
                eye,
                [0., -f64::from(s), -f64::from(c)],
                [near, far],
                Projection::Orthographic,
            )?,
        })
    }
}

/// Intersect the screen rectangle with the projected bounds of an upright object.
/// The camera must be produced by GroundCamera::view.
pub fn on_screen(camera: &CameraView, pos: [f32; 2], height: f32, radius: f32) -> bool {
    let m = camera.camera.clip_from_world();
    let x = m[0][0] * pos[0] + m[3][0];
    let feet = m[2][1] * pos[1] + m[3][1];
    let head = feet + m[1][1] * height;
    let margin_x = m[0][0].abs() * radius;
    let margin_y = m[2][1].abs() * radius;
    x.abs() <= 1. + margin_x && feet >= -1. - margin_y && head <= 1. + margin_y
}

/// Construct a world transform from translation, per-axis scale and a unit XZ bearing.
pub fn facing_transform(
    [x, y, z]: [f32; 3],
    [w, h, d]: [f32; 3],
    [sin, cos]: [f32; 2],
) -> PipelineResult<Transform> {
    // A stationary actor coincident with its target has no movement bearing.
    let (sin, cos) = if sin == 0. && cos == 0. {
        (0., 1.)
    } else {
        (sin, cos)
    };
    Ok(Transform::new([
        [cos * w, 0., sin * d, x],
        [0., h, 0., y],
        [-sin * w, 0., cos * d, z],
    ])?)
}

/// Contiguous drawing inputs for a world with persistent scenery and a bounded
/// set of moving objects. Preparation establishes the maximum at scene load.
pub struct DrawList {
    objects: Vec<RenderObject>,
    scenery_count: usize,
    limit: usize,
}

impl DrawList {
    pub fn new(mut scenery: Vec<RenderObject>, dynamic_capacity: usize) -> PipelineResult<Self> {
        let scenery_count = scenery.len();
        scenery.try_reserve_exact(dynamic_capacity)?;
        Ok(Self {
            objects: scenery,
            scenery_count,
            limit: scenery_count + dynamic_capacity,
        })
    }

    /// Start the next extraction, retaining the already prepared scenery.
    pub fn begin(&mut self) {
        self.objects.truncate(self.scenery_count);
    }

    pub fn push(&mut self, object: RenderObject) -> PipelineResult<()> {
        if self.objects.len() == self.limit {
            return Err("scene exceeded its prepared dynamic draw count".into());
        }
        self.objects.push(object);
        Ok(())
    }

    pub fn objects(&self) -> &[RenderObject] {
        &self.objects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_camera_tracks_its_target_and_projects_positive_z_down() {
        let rig = GroundCamera {
            height: 12.,
            distance: 20.,
            pitch_radians: 0.8,
            depth_range: [0.1, 100.],
        };
        let view = rig.view(ViewId(7), [12., 9.], [960, 480]).unwrap();
        assert!(on_screen(&view, [12., 9.], 2., 0.4));
        assert!(!on_screen(&view, [25., 9.], 2., 0.4));
        assert!(!on_screen(&view, [12., -12.], 2., 0.4));
        assert!(rig.view(ViewId(7), [12., 9.], [0, 480]).is_err());
        assert!(view.camera.clip_from_world()[2][1] > 0.);
    }
}

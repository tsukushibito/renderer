//! Orthographic framing and extraction of objects positioned on the XZ plane.
use crate::{
    math::{LocalTransform, Vec2, Vec3},
    scene::Registry,
};
pub use renderer::pbr::PointLight;
use renderer::{
    pbr::{Projection, Transform, ViewInput},
    pipeline::{CameraView, PipelineResult, RenderObject, ViewId},
};
use serde::{Deserialize, Serialize};

/// Scene components shared by game rendering, the inspector and offline validation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointLamp {
    pub range: f32,
    pub tint: [f32; 3],
    pub candela: f32,
}
impl PointLamp {
    pub fn at(self, position: Vec3) -> Result<PointLight, String> {
        PointLight::from_candela(position.to_array(), self.range, self.tint, self.candela)
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingCamera {
    pub height: f32,
    pub distance: f32,
    pub pitch_degrees: f32,
    pub depth_range: [f32; 2],
    /// Fraction of target movement relative to the camera's placed anchor.
    pub follow: f32,
    /// Maximum displacement of the target from the frame center, in half-extents.
    pub screen_margin: f32,
}
impl TrackingCamera {
    fn rig(self) -> GroundCamera {
        GroundCamera {
            height: self.height,
            distance: self.distance,
            pitch_radians: self.pitch_degrees.to_radians(),
            depth_range: self.depth_range,
        }
    }
    fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.follow) || !(0.0..=1.0).contains(&self.screen_margin) {
            return Err("follow and screen_margin must be in [0,1]".into());
        }
        self.rig()
            .view(ViewId(0), Vec2::ZERO, [1, 1])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn follow_view(
        self,
        id: ViewId,
        anchor: Vec2,
        target: Vec2,
        bounds: Vec2,
        extent: [u32; 2],
    ) -> PipelineResult<CameraView> {
        if extent.contains(&0)
            || !target
                .to_array()
                .into_iter()
                .chain(anchor.to_array())
                .all(f32::is_finite)
            || !bounds
                .to_array()
                .into_iter()
                .all(|v| v >= 0. && v.is_finite())
        {
            return Err("camera needs a nonempty viewport and finite ground bounds".into());
        }
        let rig = self.rig();
        let half = self.height * 0.5;
        let limits = [
            half * extent[0] as f32 / extent[1] as f32,
            half / rig.pitch_radians.sin(),
        ];
        let mut focus = anchor;
        for i in 0..2 {
            let margin = limits[i] * self.screen_margin;
            if !margin.is_finite() || margin < 0. || !self.follow.is_finite() {
                return Err("invalid ground camera framing".into());
            }
            focus[i] = (anchor[i] + (target[i] - anchor[i]) * self.follow)
                .clamp(target[i] - margin, target[i] + margin)
                .clamp(-bounds[i], bounds[i]);
        }
        rig.view(id, focus, extent)
    }
}

pub fn register_components<C: 'static>(registry: &mut Registry<C>) -> Result<(), String> {
    registry.register::<PointLamp>("PointLight", &["range", "tint", "candela"], |lamp| {
        lamp.at(Vec3::ZERO).map(|_| ())
    })?;
    registry.register(
        "GroundCamera",
        &[
            "height",
            "distance",
            "pitch_degrees",
            "depth_range",
            "follow",
            "screen_margin",
        ],
        TrackingCamera::validate,
    )
}

/// Camera looks toward negative Z from above the ground. Height and distance are metres.
#[derive(Clone, Copy, Debug)]
pub struct GroundCamera {
    pub height: f32,
    pub distance: f32,
    pub pitch_radians: f32,
    pub depth_range: [f32; 2],
}

impl GroundCamera {
    pub fn view(self, id: ViewId, target: Vec2, extent: [u32; 2]) -> PipelineResult<CameraView> {
        let [width, height] = extent;
        if width == 0 || height == 0 {
            return Err("camera requires a nonempty viewport".into());
        }
        let [x, z] = target.to_array();
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
pub fn on_screen(camera: &CameraView, pos: Vec2, height: f32, radius: f32) -> bool {
    let m = camera.camera.clip_from_world();
    let x = m[0][0] * pos[0] + m[3][0];
    let feet = m[2][1] * pos[1] + m[3][1];
    let head = feet + m[1][1] * height;
    let margin_x = m[0][0].abs() * radius;
    let margin_y = m[2][1].abs() * radius;
    x.abs() <= 1. + margin_x && feet >= -1. - margin_y && head <= 1. + margin_y
}

/// Prepare a standalone model placement for rendering. The renderer requires an
/// invertible linear part so it can transform normals.
pub fn render_transform(placement: LocalTransform) -> PipelineResult<Transform> {
    let columns = placement.affine().to_cols_array_2d();
    Ok(Transform::new(std::array::from_fn(|row| {
        columns.map(|column| column[row])
    }))?)
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
        let view = rig.view(ViewId(7), Vec2::new(12., 9.), [960, 480]).unwrap();
        assert!(on_screen(&view, Vec2::new(12., 9.), 2., 0.4));
        assert!(!on_screen(&view, Vec2::new(25., 9.), 2., 0.4));
        assert!(!on_screen(&view, Vec2::new(12., -12.), 2., 0.4));
        assert!(rig.view(ViewId(7), Vec2::new(12., 9.), [0, 480]).is_err());
        assert!(view.camera.clip_from_world()[2][1] > 0.);
    }
}

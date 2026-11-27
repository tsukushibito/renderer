use super::{FilmCamera, InfiniteSky, PerspectiveView};
use crate::{ObjectBytes, ObjectStore, invalid};
use glam::{Mat3, Mat4, Vec2, Vec3};
use std::{io, sync::Arc};

/// World-space thin-lens ray in the same metre coordinates as scene placements.
pub struct FilmRay {
    pub origin: Vec3,
    pub direction: Vec3,
}

/// A prepared film, lens and linear-sRGB sensor response.
pub struct FilmProjection {
    basis: FilmRayBasis,
    sensor: Mat3,
}

/// World-space vectors for evaluating the same film projection on a device.
pub struct FilmRayBasis {
    pub eye: Vec3,
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub centre: Vec2,
    pub pixel_scale: f32,
    pub radius: f32,
    pub focus: f32,
}

impl FilmProjection {
    pub fn ray_basis(&self) -> &FilmRayBasis {
        &self.basis
    }

    pub fn new(view: &PerspectiveView, film: &FilmCamera, extent: [u32; 2]) -> io::Result<Self> {
        let eye = Vec3::from(view.position);
        let forward = Vec3::from(view.forward)
            .try_normalize()
            .ok_or_else(|| invalid("invalid film direction"))?;
        let right = forward
            .cross(view.up.into())
            .try_normalize()
            .ok_or_else(|| invalid("invalid film up vector"))?;
        let up = right.cross(forward);
        let centre = Vec2::new(extent[0] as f32, extent[1] as f32) * 0.5;
        if !eye.is_finite()
            || extent.contains(&0)
            || !(view.vertical_fov_degrees > 0. && view.vertical_fov_degrees < 180.)
            || !film.lens_radius_metres.is_finite()
            || film.lens_radius_metres < 0.
            || !film.focus_metres.is_finite()
            || film.focus_metres <= 0.
            || !film.iso.is_finite()
            || film.iso <= 0.
        {
            return Err(invalid("invalid film geometry or exposure"));
        }
        Ok(Self {
            basis: FilmRayBasis {
                eye,
                forward,
                up,
                right: if view.mirror_x { -right } else { right },
                centre,
                pixel_scale: (0.5 * view.vertical_fov_degrees.to_radians()).tan() / centre.y,
                radius: film.lens_radius_metres,
                focus: film.focus_metres,
            },
            sensor: white_balance(film.white_balance_kelvin)? * (film.iso / 100.),
        })
    }

    /// Continuous film coordinates have their origin at the top left. The
    /// aperture sample is a point on the unit disk, with zero at lens centre.
    pub fn ray(&self, pixel: Vec2, aperture: Vec2) -> FilmRay {
        let film = (pixel - self.basis.centre) * self.basis.pixel_scale;
        let pinhole = self.basis.forward + film.x * self.basis.right - film.y * self.basis.up;
        let offset =
            self.basis.radius * (aperture.x * self.basis.right + aperture.y * self.basis.up);
        FilmRay {
            origin: self.basis.eye + offset,
            direction: (pinhole - offset / self.basis.focus).normalize(),
        }
    }

    /// Rasterize one sample of the same lens as `ray`. Translate the eye
    /// parallel to the film and shift the frustum to keep the focal plane fixed.
    /// The pixel offset is relative to its centre, positive right and down.
    pub fn raster_sample(
        &self,
        centred: &sashiko_renderer::pipeline::CameraView,
        pixel_offset: Vec2,
        aperture: Vec2,
    ) -> sashiko_renderer::pipeline::PipelineResult<sashiko_renderer::pbr::ViewInput> {
        use sashiko_renderer::pbr::{Projection, ViewInput};
        let base = centred.camera;
        let matrix = Mat4::from_cols_array_2d(&base.clip_from_world()).as_dmat4();
        let offset = (self.basis.radius
            * (aperture.x * self.basis.right + aperture.y * self.basis.up))
            .as_dvec3();
        let focus =
            self.basis.eye.as_dvec3() + self.basis.forward.as_dvec3() * f64::from(self.basis.focus);
        let w = (matrix * focus.extend(1.)).w;
        let lens_shift = (matrix * offset.extend(0.)).truncate().truncate() / w;
        let shift = lens_shift - (pixel_offset / self.basis.centre).as_dvec2();
        let sampled = glam::DMat4::from_translation(shift.extend(0.))
            * matrix
            * glam::DMat4::from_translation(-offset);
        Ok(ViewInput::new(
            sampled.as_mat4().to_cols_array_2d(),
            (self.basis.eye.as_dvec3() + offset).as_vec3().into(),
            base.forward(),
            base.near_far(),
            Projection::Perspective,
        )?)
    }

    /// Apply the exposure and chromatic adaptation once to integrated linear RGB.
    pub fn develop(&self, radiance: Vec3) -> Vec3 {
        self.sensor * radiance
    }

    /// Restrict this sample to a top-origin rectangle on the original film.
    /// Every retained pixel has the same ray, subpixel offset and lens sample.
    pub fn crop(
        full: sashiko_renderer::pipeline::CameraView,
        [x, y, width, height]: [u32; 4],
    ) -> sashiko_renderer::pipeline::PipelineResult<sashiko_renderer::pipeline::CameraView> {
        use sashiko_renderer::pbr::{Projection, ViewInput};
        let [w, h] = full.extent;
        if width == 0
            || height == 0
            || x.checked_add(width).is_none_or(|v| v > w)
            || y.checked_add(height).is_none_or(|v| v > h)
        {
            return Err("crop exceeds the full film".into());
        }
        let base = full.camera;
        let mut matrix = Mat4::from_cols_array_2d(&base.clip_from_world())
            .as_dmat4()
            .to_cols_array_2d();
        for col in &mut matrix {
            col[0] = (f64::from(w) * col[0] + (f64::from(w - width) - 2. * f64::from(x)) * col[3])
                / f64::from(width);
            col[1] = (f64::from(h) * col[1] + (f64::from(h - height) - 2. * f64::from(y)) * col[3])
                / f64::from(height);
        }
        let camera = ViewInput::new(
            matrix.map(|c| c.map(|v| v as f32)),
            base.eye(),
            base.forward(),
            base.near_far(),
            Projection::Perspective,
        )?;
        Ok(sashiko_renderer::pipeline::CameraView {
            extent: [width, height],
            camera,
            ..full
        })
    }

    pub(super) fn output<'a>(
        &self,
        sky: &'a sashiko_renderer::core::graph::ImportImage,
        light_from_world: Mat3,
        scale: f32,
        view: &sashiko_renderer::pipeline::CameraView,
    ) -> sashiko_renderer::passes::output::FilmOutput<'a> {
        // Unproject the frame's actual projection, including a moved camera,
        // without performing perspective division at the far plane.
        let inverse = Mat4::from_cols_array_2d(&view.camera.clip_from_world())
            .as_dmat4()
            .inverse();
        let eye = Vec3::from(view.camera.eye()).as_dvec3();
        let direction = |v: glam::DVec4| light_from_world * (v.truncate() - eye * v.w).as_vec3();
        let corner = direction(inverse * glam::DVec4::new(-1., -1., 1., 1.));
        let dx = direction(inverse.x_axis) * (2. / view.extent[0] as f32);
        let dy = direction(inverse.y_axis) * (2. / view.extent[1] as f32);
        sashiko_renderer::passes::output::FilmOutput {
            sky,
            light_scale: scale,
            ray_corner: corner.into(),
            ray_step_x: dx.into(),
            ray_step_y: dy.into(),
            sensor: self.sensor.transpose().to_cols_array_2d(),
        }
    }
}

fn white_balance(kelvin: Option<f32>) -> io::Result<Mat3> {
    let Some(t) = kelvin else {
        return Ok(Mat3::IDENTITY);
    };
    let cct = t * 1.4388 / 1.4380;
    // CIE daylight locus and the Planckian locus fit below daylight temperatures.
    if !(1667.0..=25000.).contains(&cct) {
        return Err(invalid(
            "white balance outside supported CCT range 1667–25000 K",
        ));
    }
    let k = 1000. / cct;
    let (x, y) = if cct < 4000. {
        let x = ((-0.2661239 * k - 0.234358) * k + 0.8776956) * k + 0.179910;
        let y = if cct <= 2222. {
            ((-1.1063814 * x - 1.3481102) * x + 2.1855583) * x - 0.20219683
        } else {
            ((-0.9549476 * x - 1.3741859) * x + 2.09137) * x - 0.16748867
        };
        (x, y)
    } else {
        let x = if cct <= 7000. {
            ((-4.607 * k + 2.9678) * k + 0.09911) * k + 0.244063
        } else {
            ((-2.0064 * k + 1.9018) * k + 0.24748) * k + 0.23704
        };
        (x, (-3. * x + 2.870) * x - 0.275)
    };
    let xyz = |x: f32, y: f32| Vec3::new(x / y, 1., (1. - x - y) / y);
    let bradford = Mat3::from_cols_array(&[
        0.8951, -0.7502, 0.0389, 0.2664, 1.7135, -0.0685, -0.1614, 0.0367, 1.0296,
    ]);
    let rgb_to_xyz = Mat3::from_cols_array(&[
        0.4124564, 0.2126729, 0.0193339, 0.3575761, 0.7151522, 0.119192, 0.1804375, 0.072175,
        0.9503041,
    ]);
    let scale = (bradford * xyz(0.3127, 0.3290)) / (bradford * xyz(x, y));
    let rgb_to_lms = bradford * rgb_to_xyz;
    Ok(rgb_to_lms.inverse() * Mat3::from_diagonal(scale) * rgb_to_lms)
}

/// PBRT equal-area infinite light in a declared world orientation.
pub struct SkyRadiance {
    data: Arc<ObjectBytes>,
    size: usize,
    light_from_world: Mat3,
    scale: f32,
}

impl SkyRadiance {
    pub fn open(store: &ObjectStore, sky: &InfiniteSky) -> io::Result<Self> {
        let map = &sky.equal_area_radiance;
        map.validate()?;
        let matrix = Mat3::from_mat4(Mat4::from_cols_array_2d(&sky.world_from_light)).inverse();
        if !matrix.is_finite()
            || map.width != map.height
            || map.mip_levels != 1
            || !sky.scale.is_finite()
            || sky.scale < 0.
        {
            return Err(invalid("invalid infinite light transform or map"));
        }
        Ok(Self {
            data: store.load(&map.data)?,
            size: map.width as usize,
            light_from_world: matrix,
            scale: sky.scale,
        })
    }

    /// Nearest source texel as used by PBRT's image infinite light. All texels
    /// subtend the same solid angle, including those adjacent to the octants.
    pub fn sample(&self, world_direction: Vec3) -> Vec3 {
        let d = (self.light_from_world * world_direction).normalize();
        let r = (1. - d.z.abs()).max(0.).sqrt();
        let a = d.y.abs().atan2(d.x.abs()) * std::f32::consts::FRAC_2_PI;
        let (u, v) = (r * (1. - a), r * a);
        let (u, v) = if d.z < 0. { (1. - v, 1. - u) } else { (u, v) };
        let coord = |v: f32, sign: f32| {
            (((0.5 + 0.5 * v.copysign(sign)) * self.size as f32) as usize).min(self.size - 1)
        };
        let offset = 8 * (self.size * coord(v, d.y) + coord(u, d.x));
        let channel = |i: usize| {
            half::f16::from_le_bytes([self.data[offset + i], self.data[offset + i + 1]]).to_f32()
        };
        Vec3::new(channel(0), channel(2), channel(4)) * self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlobRef, scene::EnvironmentImage};

    #[test]
    fn lens_rays_converge_on_the_focal_plane_and_exposure_is_linear() -> io::Result<()> {
        let view = PerspectiveView {
            position: [0.; 3],
            forward: [0., 0., -1.],
            up: [0., 1., 0.],
            vertical_fov_degrees: 60.,
            mirror_x: false,
        };
        let film = FilmCamera {
            lens_radius_metres: 0.01,
            focus_metres: 4.,
            iso: 150.,
            white_balance_kelvin: None,
            film_extent: [600, 400],
            samples_per_pixel: 16,
        };
        let camera = FilmProjection::new(&view, &film, film.film_extent)?;
        let at_depth = |ray: FilmRay, z: f32| ray.origin + ray.direction * (-z / ray.direction.z);
        let pixel = Vec2::new(450., 75.);
        let from = |lens| camera.ray(pixel, lens);
        let focus = at_depth(from(Vec2::X), 4.);
        assert!(focus.x > 0. && focus.y > 0.);
        assert!(focus.distance(at_depth(from(-Vec2::X), 4.)) < 1e-5);
        assert!(
            (at_depth(from(Vec2::X), 2.).distance(at_depth(from(-Vec2::X), 2.)) - 0.01).abs()
                < 1e-5
        );
        assert_eq!(camera.develop(Vec3::ONE), Vec3::splat(1.5));
        Ok(())
    }

    #[test]
    fn rotated_equal_area_sky_maps_all_six_cardinals() -> io::Result<()> {
        let size = 9;
        let mut pixels = Vec::new();
        for y in 0..size {
            for x in 0..size {
                for value in [x as f32, y as f32, 0., 1.] {
                    pixels.extend(half::f16::from_f32(value).to_le_bytes());
                }
            }
        }
        let data = BlobRef::of(&pixels);
        let rotation = Mat4::from_cols(
            Vec3::Y.extend(0.),
            Vec3::Z.extend(0.),
            Vec3::X.extend(0.),
            glam::Vec4::W,
        );
        let store = ObjectStore::with_reader(".", move |_, _| Ok(pixels.clone()));
        let sky = SkyRadiance::open(
            &store,
            &InfiniteSky {
                equal_area_radiance: EnvironmentImage {
                    width: size,
                    height: size,
                    mip_levels: 1,
                    data,
                },
                world_from_light: rotation.to_cols_array_2d(),
                scale: 2.,
            },
        )?;
        for (direction, expected) in [
            (Vec3::X, [8., 4.]),
            (-Vec3::X, [0., 4.]),
            (Vec3::Y, [4., 8.]),
            (-Vec3::Y, [4., 0.]),
            (Vec3::Z, [4., 4.]),
            (-Vec3::Z, [8., 8.]),
        ] {
            let sample = sky.sample(rotation.transform_vector3(direction));
            if direction == -Vec3::Z {
                // All four corners represent the south pole.
                assert!([0., 16.].contains(&sample.x) && [0., 16.].contains(&sample.y));
            } else {
                assert_eq!(sample, 2. * Vec3::new(expected[0], expected[1], 0.));
            }
        }
        Ok(())
    }
}

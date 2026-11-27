use sashiko_asset::{
    ObjectStore,
    scene::{DisplayScene, FilmProjection, ScenePackage, SkyRadiance},
};
use sashiko_renderer::pipeline::PipelineResult;
use std::{fs::File, io::BufWriter, num::NonZeroU32, path::Path, time::Duration};

fn radical_inverse(mut n: u32, base: u32) -> f32 {
    let mut result = 0.;
    let mut weight = 1.;
    while n > 0 {
        weight /= base as f32;
        result += (n % base) as f32 * weight;
        n /= base;
    }
    result
}

/// Offline accumulation of actual renderer frames in linear film space.
pub(super) fn capture(
    package: &Path,
    name: &str,
    samples: NonZeroU32,
    exr: &Path,
    png: &Path,
) -> PipelineResult<()> {
    let (extent, lens) = {
        let scene = ScenePackage::open(package)?;
        let film = scene
            .physical
            .as_ref()
            .and_then(|p| p.cameras.get(name))
            .ok_or("view has no physical film")?;
        (
            film.film_extent,
            FilmProjection::new(scene.view(name)?, film, film.film_extent)?,
        )
    };
    let (scene, mut renderer) = DisplayScene::headless(package, name, extent, super::backend())?;
    let pixels = (extent[0] as usize)
        .checked_mul(extent[1] as usize)
        .ok_or("film size overflow")?;
    let mut sum = vec![[0f64; 3]; pixels];
    let mut rgba = vec![0; pixels.checked_mul(8).ok_or("film size overflow")?];
    for index in 1..=samples.get() {
        if samples.get() > 1 {
            let jitter = [
                radical_inverse(index, 2) - 0.5,
                radical_inverse(index, 3) - 0.5,
            ];
            let radius = radical_inverse(index, 5).sqrt();
            let (s, c) = (std::f32::consts::TAU * radical_inverse(index, 7)).sin_cos();
            renderer.set_camera(lens.raster_sample(
                &scene.camera,
                jitter.into(),
                [radius * c, radius * s].into(),
            )?);
        }
        let mut frame = renderer.begin_frame()?;
        scene.draw(&mut frame)?;
        let ticket = frame.capture_linear()?;
        if !renderer.read_capture(&ticket, &mut rgba, Duration::from_secs(120))? {
            return Err("linear film capture timed out".into());
        }
        for (pixel, total) in rgba.as_chunks::<8>().0.iter().zip(&mut sum) {
            for (pair, channel) in pixel.as_chunks::<2>().0.iter().zip(total) {
                *channel += exr::prelude::f16::from_le_bytes(*pair).to_f64();
            }
        }
    }
    super::finish(scene, renderer.into_runtime()?)?;
    let linear: Vec<_> = sum
        .into_iter()
        .map(|p| p.map(|v| (v / f64::from(samples.get())) as f32))
        .collect();
    write_film(extent[0], extent[1], &linear, exr, png)
}

/// Isolate the authored infinite light and sensor at native film resolution.
pub(super) fn sky(package: &Path, name: &str, exr: &Path, png: &Path) -> PipelineResult<()> {
    let scene = ScenePackage::open(package)?;
    let physical = scene
        .physical
        .as_ref()
        .ok_or("scene has no physical settings")?;
    let film = physical
        .cameras
        .get(name)
        .ok_or("view has no source film")?;
    let camera = FilmProjection::new(scene.view(name)?, film, film.film_extent)?;
    let sky = SkyRadiance::open(
        &ObjectStore::new(package),
        physical.sky.as_ref().ok_or("scene has no infinite light")?,
    )?;
    let [width, height] = film.film_extent;
    let mut linear = Vec::new();
    linear.try_reserve_exact(
        (width as usize)
            .checked_mul(height as usize)
            .ok_or("film size overflow")?,
    )?;
    for y in 0..height {
        for x in 0..width {
            let ray = camera.ray([x as f32 + 0.5, y as f32 + 0.5].into(), [0., 0.].into());
            linear.push(camera.develop(sky.sample(ray.direction)).to_array());
        }
    }
    write_film(width, height, &linear, exr, png)
}

fn write_film(
    width: u32,
    height: u32,
    linear: &[[f32; 3]],
    exr: &Path,
    png: &Path,
) -> PipelineResult<()> {
    if linear.as_flattened().iter().any(|value| !value.is_finite()) {
        return Err("non-finite linear film sample".into());
    }
    exr::prelude::write_rgb_file(exr, width as usize, height as usize, |x, y| {
        let [r, g, b] = linear[y * width as usize + x];
        (r, g, b)
    })?;
    let display: Vec<u8> = linear
        .iter()
        .flatten()
        .map(|&v| {
            let t = v.clamp(0., 1.);
            let encoded = if t <= 0.0031308 {
                12.92 * t
            } else {
                1.055 * t.powf(1. / 2.4) - 0.055
            };
            (encoded * 255.).round() as u8
        })
        .collect();
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(png)?), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&display)?;
    Ok(())
}

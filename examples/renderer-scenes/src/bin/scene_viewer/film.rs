use sashiko_asset::{
    ObjectStore,
    scene::{FilmProjection, ScenePackage, SkyRadiance},
};
use sashiko_renderer::pipeline::PipelineResult;
use std::{fs::File, io::BufWriter, path::Path};

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

use sashiko_asset::scene::{FilmProjection, ScenePackage};
use sashiko_lighting_bake::{FilmLighting, FilmSampler, FilmTracer};
use sashiko_renderer::{core::vulkan::ValidationMode, pipeline::PipelineResult};
use std::{fs::File, io::BufWriter, num::NonZeroU32, path::Path};

pub(super) enum Output<'a> {
    Coverage(&'a Path),
    Radiance {
        lighting: FilmLighting,
        exr: &'a Path,
        png: &'a Path,
    },
}

pub(super) fn capture(
    package: &Path,
    name: &str,
    spp: NonZeroU32,
    rect: [u32; 4],
    output: Output<'_>,
) -> PipelineResult<()> {
    let [x0, y0, w, h] = rect;
    let (extent, lens) = {
        let s = ScenePackage::open(package)?;
        let film = s
            .physical
            .as_ref()
            .and_then(|p| p.cameras.get(name))
            .ok_or("missing physical film")?;
        (
            film.film_extent,
            FilmProjection::new(s.view(name)?, film, film.film_extent)?,
        )
    };
    if w == 0
        || h == 0
        || x0.checked_add(w).is_none_or(|x| x > extent[0])
        || y0.checked_add(h).is_none_or(|y| y > extent[1])
    {
        return Err("invalid film rectangle".into());
    }
    let validation = if cfg!(feature = "development-validation") {
        ValidationMode::Required
    } else {
        ValidationMode::Disabled
    };
    let mut tracer = match output {
        Output::Coverage(_) => FilmTracer::open(package, validation)?,
        Output::Radiance { lighting, .. } => {
            FilmTracer::for_radiance(package, validation, lighting)?
        }
    };
    let pixels = usize::try_from(u64::from(w) * u64::from(h))?;
    let n = usize::try_from(spp.get())?;
    let sampler = FilmSampler::default();
    if let Output::Radiance { exr, png, .. } = output {
        let mut linear = tracer.capture_radiance(&lens, rect, spp, &sampler)?;
        tracer.finish()?;
        for rgb in &mut linear {
            *rgb = lens.develop((*rgb).into()).to_array();
        }
        return super::film::write_film(w, h, &linear, exr, png);
    }
    let Output::Coverage(path) = output else {
        unreachable!()
    };
    let jobs = pixels.checked_mul(n).ok_or("film sample count overflow")?;
    let mut counts = vec![0u32; pixels];
    let mut rays = Vec::with_capacity(32768);
    let mut hits = Vec::with_capacity(32768);
    for start in (0..jobs).step_by(32768) {
        rays.clear();
        for j in start..start.saturating_add(32768).min(jobs) {
            let pixel = j / n;
            let x = x0 + (pixel % w as usize) as u32;
            let y = y0 + (pixel / w as usize) as u32;
            rays.push(sampler.ray(&lens, x, y, (j % n) as u32));
        }
        hits.resize_with(rays.len(), || None);
        tracer.primary(&rays, &mut hits)?;
        for (i, hit) in hits.iter().enumerate() {
            counts[(start + i) / n] += u32::from(hit.is_some());
        }
    }
    tracer.finish()?;
    let bytes: Vec<_> = counts
        .into_iter()
        .flat_map(|v| ((f64::from(v) / f64::from(spp.get()) * 65535.).round() as u16).to_be_bytes())
        .collect();
    let mut png = png::Encoder::new(BufWriter::new(File::create(path)?), w, h);
    png.set_color(png::ColorType::Grayscale);
    png.set_depth(png::BitDepth::Sixteen);
    png.write_header()?.write_image_data(&bytes)?;
    Ok(())
}

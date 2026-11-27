use sashiko_asset::scene::{FilmProjection, ScenePackage};
use sashiko_lighting_bake::FilmTracer;
use sashiko_renderer::{core::vulkan::ValidationMode, pipeline::PipelineResult};
use std::{fs::File, io::BufWriter, num::NonZeroU32, path::Path};

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^ (x >> 16)
}

/// Invert the separable PBRT default reconstruction filter. Each sample then
/// has equal weight in the pixel. The tabulation integrates to both boundaries.
fn filter_cdf() -> Vec<f32> {
    let mut sum = 0.;
    let mut cdf = vec![0.];
    let bins = 2048;
    for i in 0..bins {
        let x = (i as f64 + 0.5) / f64::from(bins) * 3.0 - 1.5;
        sum += (-2. * x * x).exp() - (-4.5f64).exp();
        cdf.push(sum as f32);
    }
    for v in &mut cdf {
        *v /= sum as f32;
    }
    cdf
}

fn filter_offset(cdf: &[f32], u: f32) -> f32 {
    let hi = cdf.partition_point(|&v| v <= u).clamp(1, cdf.len() - 1);
    let lo = hi - 1;
    let fraction = (u - cdf[lo]) / (cdf[hi] - cdf[lo]).max(f32::MIN_POSITIVE);
    (lo as f32 + fraction) * 3. / (cdf.len() - 1) as f32 - 1.5
}

pub(super) fn coverage(
    package: &Path,
    name: &str,
    spp: NonZeroU32,
    rect: [u32; 4],
    path: &Path,
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
        return Err("invalid coverage rectangle".into());
    }
    let mut tracer = FilmTracer::open(
        package,
        if cfg!(feature = "development-validation") {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )?;
    let pixels = usize::try_from(u64::from(w) * u64::from(h))?;
    let n = usize::try_from(spp.get())?;
    let jobs = pixels
        .checked_mul(n)
        .ok_or("coverage sample count overflow")?;
    let cdf = filter_cdf();
    let mut counts = vec![0u32; pixels];
    let mut rays = Vec::with_capacity(32768);
    let mut hits = Vec::with_capacity(32768);
    for start in (0..jobs).step_by(32768) {
        rays.clear();
        for j in start..start.saturating_add(32768).min(jobs) {
            let pixel = j / n;
            let i = (j % n) as u32;
            let x = x0 + (pixel % w as usize) as u32;
            let y = y0 + (pixel / w as usize) as u32;
            let key = hash(x ^ hash(y));
            let component = |base: u32| {
                let shift = (hash(key ^ base) >> 8) as f32 * (1. / 16777216.);
                (super::film::radical_inverse(i, base) + shift).fract()
            };
            let xy = [
                x as f32 + 0.5 + filter_offset(&cdf, component(2)),
                y as f32 + 0.5 + filter_offset(&cdf, component(3)),
            ];
            let radius = component(5).sqrt();
            let (sn, cs) = (std::f32::consts::TAU * component(7)).sin_cos();
            rays.push((
                lens.ray(xy.into(), [radius * cs, radius * sn].into()),
                hash(key ^ hash(i)),
            ));
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

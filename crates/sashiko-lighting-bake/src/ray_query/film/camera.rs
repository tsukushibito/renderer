use super::{FILM_FILTER_BINS, FilmTracer, PIXELS_PER_DISPATCH, Result};
use sashiko_asset::scene::{FilmProjection, FilmRay};
use std::num::NonZeroU32;

/// Shared reconstruction filter and shifted Halton coordinates for visibility
/// and radiance. Samples use absolute film coordinates, including cropped jobs.
pub struct FilmSampler {
    cdf: Vec<f32>,
}

impl Default for FilmSampler {
    fn default() -> Self {
        Self { cdf: filter_cdf() }
    }
}

impl FilmSampler {
    pub fn ray(&self, lens: &FilmProjection, x: u32, y: u32, i: u32) -> (FilmRay, u32) {
        let key = hash(x ^ hash(y));
        let component = |base: u32| {
            let shift = (hash(key ^ base) >> 8) as f32 * (1. / 16777216.);
            (radical_inverse(i, base) + shift).fract()
        };
        let xy = [
            x as f32 + 0.5 + filter_offset(&self.cdf, component(2)),
            y as f32 + 0.5 + filter_offset(&self.cdf, component(3)),
        ];
        let radius = component(5).sqrt();
        let (sn, cs) = (std::f32::consts::TAU * component(7)).sin_cos();
        (
            lens.ray(xy.into(), [radius * cs, radius * sn].into()),
            hash(key ^ hash(i)),
        )
    }
}

impl FilmTracer {
    /// Trace a rectangle on the GPU and return scene-linear averages in row-major order.
    pub fn capture_radiance(
        &mut self,
        lens: &FilmProjection,
        [x, y, w, h]: [u32; 4],
        samples: NonZeroU32,
        sampler: &FilmSampler,
    ) -> Result<Vec<[f32; 3]>> {
        if !self.shaded {
            return Err("radiance requires prepared physical materials".into());
        }
        let basis = lens.ray_basis();
        if w == 0
            || h == 0
            || x.checked_add(w)
                .is_none_or(|v| v as f32 > basis.centre.x * 2.)
            || y.checked_add(h)
                .is_none_or(|v| v as f32 > basis.centre.y * 2.)
        {
            return Err("invalid film rectangle".into());
        }
        let pixels = w.checked_mul(h).ok_or("film tile too large")? as usize;
        let mut sums = vec![[0f64; 3]; pixels];
        // Six 16-byte vectors followed by 64 Halton tuples and the filter CDF.
        let mut input = vec![0u8; 96 + 64 * 16];
        for (i, word) in [x, y, w].into_iter().enumerate() {
            input[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
        }
        let vectors = [
            basis.eye.extend(basis.radius),
            basis.forward.extend(basis.focus),
            basis.right.extend(basis.pixel_scale),
            basis.up.extend(0.),
        ];
        for (dst, value) in input[24..96].as_chunks_mut::<4>().0.iter_mut().zip(
            basis
                .centre
                .to_array()
                .into_iter()
                .chain(vectors.into_iter().flat_map(|v| v.to_array())),
        ) {
            *dst = value.to_le_bytes();
        }
        for value in &sampler.cdf {
            input.extend(value.to_le_bytes());
        }
        // One film pixel projected at unit distance, scaled to the sample footprint.
        input[92..96].copy_from_slice(
            &(basis.pixel_scale * (1. / (samples.get() as f32).sqrt()).max(0.125)).to_le_bytes(),
        );
        for first in (0..samples.get()).step_by(64) {
            let lanes = (samples.get() - first).min(64);
            input[16..20].copy_from_slice(&first.to_le_bytes());
            input[20..24].copy_from_slice(&lanes.to_le_bytes());
            for lane in 0..lanes {
                let offset = 96 + lane as usize * 16;
                for (i, base) in [2, 3, 5, 7].into_iter().enumerate() {
                    input[offset + 4 * i..offset + 4 * i + 4]
                        .copy_from_slice(&radical_inverse(first + lane, base).to_le_bytes());
                }
            }
            for (tile, chunk) in sums.chunks_mut(PIXELS_PER_DISPATCH).enumerate() {
                let pixel = u32::try_from(tile * PIXELS_PER_DISPATCH)?;
                input[12..16].copy_from_slice(&pixel.to_le_bytes());
                self.input.write(&input)?;
                self.dispatch(chunk.len() as u32)?;
                let records = self.output.read(chunk.len() * 16)?.as_chunks::<16>().0;
                for (sum, record) in chunk.iter_mut().zip(records) {
                    for (channel, bytes) in sum.iter_mut().zip(record.as_chunks::<4>().0) {
                        *channel += f64::from(f32::from_le_bytes(*bytes));
                    }
                }
            }
        }
        Ok(sums
            .into_iter()
            .map(|sum| sum.map(|v| (v / f64::from(samples.get())) as f32))
            .collect())
    }
}

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
    let bins = FILM_FILTER_BINS as u32;
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

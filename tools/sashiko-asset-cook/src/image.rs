use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub(crate) enum ImageRole {
    Color,
    Normal,
    Orm,
    Scalar,
}

impl ImageRole {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Normal => "normal",
            Self::Orm => "orm",
            Self::Scalar => "scalar",
        }
    }
}

use png::{ColorType, Decoder, Transformations};
use sashiko_asset::ImageBlob;
use std::{error::Error, io::Cursor, ops::Range};

pub(super) fn cook(bytes: &[u8], role: ImageRole) -> Result<Vec<u8>, Box<dyn Error>> {
    let image = decode(bytes)?;
    cook_rgba(image.width, image.height, image.rgba, role)
}

pub(crate) struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    source: ColorType,
}

impl Decoded {
    /// PBRT's float PNG map selects RGBA alpha when any texel is below one, otherwise the
    /// mean of the linear RGB channels. Gray/gray-alpha use luminance alone.
    pub fn pbrt_scalar(&self) -> Vec<u8> {
        let pixels = self.rgba.as_chunks::<4>().0;
        let alpha = self.source == ColorType::Rgba && pixels.iter().any(|p| p[3] != 255);
        let linear: [f64; 256] = std::array::from_fn(|v| 255. * srgb_to_linear(v as f64 / 255.));
        pixels
            .iter()
            .map(|p| {
                if alpha {
                    linear[p[3] as usize].round() as u8
                } else {
                    ((linear[p[0] as usize] + linear[p[1] as usize] + linear[p[2] as usize]) / 3.)
                        .round() as u8
                }
            })
            .collect()
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Decoded, Box<dyn Error>> {
    let mut decoder = Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let source = reader.info().color_type;
    if reader.info().animation_control.is_some() {
        return Err("animated PNG is unsupported for static glTF textures".into());
    }
    let size = reader.output_buffer_size().ok_or("PNG size overflow")?;
    let mut decoded = Vec::new();
    decoded.try_reserve_exact(size)?;
    decoded.resize(size, 0);
    let frame = reader.next_frame(&mut decoded)?;
    reader.finish()?;
    let width = frame.width as usize;
    let height = frame.height as usize;
    let rgba = match frame.color_type {
        ColorType::Rgba => decoded,
        ColorType::Indexed => return Err("PNG palette was not expanded".into()),
        ty => {
            let length = width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(4))
                .ok_or("RGBA image size overflow")?;
            let mut rgba = Vec::new();
            rgba.try_reserve_exact(length)?;
            for pixel in decoded[..frame.buffer_size()].chunks_exact(ty.samples()) {
                rgba.extend(match pixel {
                    [g] => [*g, *g, *g, 255],
                    [g, a] => [*g, *g, *g, *a],
                    [r, g, b] => [*r, *g, *b, 255],
                    _ => unreachable!("expanded 8-bit grayscale, grayscale/alpha or RGB"),
                });
            }
            rgba
        }
    };
    Ok(Decoded {
        width: frame.width,
        height: frame.height,
        rgba,
        source,
    })
}

pub(crate) fn cook_rgba(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    role: ImageRole,
) -> Result<Vec<u8>, Box<dyn Error>> {
    pyramid(width, height, rgba, role, false)
}

pub(crate) fn cook_cutout(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    pyramid(width, height, rgba, ImageRole::Color, true)
}

fn pyramid(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    role: ImageRole,
    cutout: bool,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut blob = ImageBlob {
        width,
        height,
        mip_levels: 1,
        srgb: matches!(role, ImageRole::Color),
        pixels: rgba,
    };
    let (mut w, mut h) = (width as usize, height as usize);
    let mut start = 0;
    while w > 1 || h > 1 {
        let next = downsample(&blob.pixels[start..], w, h, role, cutout);
        start = blob.pixels.len();
        blob.pixels.extend(next);
        blob.mip_levels += 1;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    if cutout {
        let base = (width as usize) * (height as usize);
        let target = blob.pixels[..4 * base]
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] >= 128)
            .count() as f64
            / base as f64;
        let (mut w, mut h) = (width, height);
        let mut rest = &mut blob.pixels[4 * base..];
        while w > 1 || h > 1 {
            w = (w / 2).max(1);
            h = (h / 2).max(1);
            let (level, tail) = rest.split_at_mut(w as usize * h as usize * 4);
            preserve_coverage(level, target);
            rest = tail;
        }
    }
    Ok(blob.encode()?)
}

/// Choose the nearest representable covered texel count. Prefer no change
/// when several thresholds tie. Leave fully transparent texels transparent.
fn preserve_coverage(level: &mut [u8], target: f64) {
    let pixels = level.as_chunks_mut::<4>().0;
    let mut histogram = [0usize; 256];
    for p in pixels.iter() {
        histogram[p[3] as usize] += 1;
    }
    let goal = (target * pixels.len() as f64).round() as usize;
    let mut above = 0usize;
    let mut best = (usize::MAX, usize::MAX, 128);
    for cutoff in (1usize..=256).rev() {
        if cutoff < 256 {
            above += histogram[cutoff];
        }
        best = best.min((above.abs_diff(goal), cutoff.abs_diff(128), cutoff));
    }
    let cutoff = best.2;
    if cutoff != 128 {
        for p in pixels {
            p[3] = ((usize::from(p[3]) * 128 / cutoff).min(255)) as u8;
        }
    }
}

// Each destination texel covers a box in the source. Integer overlap lengths
// include fractional border texels for odd dimensions and keep axes independent.
fn footprint(index: usize, src: usize, dst: usize) -> Range<u64> {
    let first = index as u64 * src as u64;
    first / dst as u64..(first + src as u64).div_ceil(dst as u64)
}

fn overlap(sample: u64, index: usize, src: usize, dst: usize) -> f64 {
    let dst = dst as u64;
    let first = index as u64 * src as u64;
    ((first + src as u64).min((sample + 1) * dst) - first.max(sample * dst)) as f64
}

fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f64) -> f64 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn downsample(
    pixels: &[u8],
    width: usize,
    height: usize,
    role: ImageRole,
    cutout: bool,
) -> Vec<u8> {
    let w = (width / 2).max(1);
    let h = (height / 2).max(1);
    let mut out = Vec::with_capacity(w * h * 4);
    let to_linear: [f64; 256] = std::array::from_fn(|i| {
        let value = i as f64 / 255.;
        match role {
            ImageRole::Color => srgb_to_linear(value),
            ImageRole::Normal => 2. * value - 1.,
            ImageRole::Orm | ImageRole::Scalar => value,
        }
    });
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.; 4];
            for sy in footprint(y, height, h) {
                let wy = overlap(sy, y, height, h);
                for sx in footprint(x, width, w) {
                    let weight = wy * overlap(sx, x, width, w);
                    let offset = (sy as usize * width + sx as usize) * 4;
                    let alpha = f64::from(pixels[offset + 3]) / 255.;
                    let rgb_weight = if cutout { weight * alpha } else { weight };
                    for channel in 0..3 {
                        sum[channel] += rgb_weight * to_linear[pixels[offset + channel] as usize];
                    }
                    sum[3] += weight * alpha;
                }
            }
            let [r, g, b, alpha] = sum.map(|v| v / (width as f64 * height as f64));
            let rgb = match role {
                ImageRole::Color if cutout && alpha > 0. => {
                    [r, g, b].map(|v| linear_to_srgb(v / alpha))
                }
                ImageRole::Color => [r, g, b].map(linear_to_srgb),
                ImageRole::Orm | ImageRole::Scalar => [r, g, b],
                ImageRole::Normal => {
                    let length = (r * r + g * g + b * b).sqrt();
                    if length > 1e-12 {
                        [r, g, b].map(|v| 0.5 + 0.5 * v / length)
                    } else {
                        [0.5, 0.5, 1.]
                    }
                }
            };
            out.extend([rgb[0], rgb[1], rgb[2], alpha].map(|v| (v * 255.).round() as u8));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_png_uses_the_source_channels_and_transfer_function() {
        let mut image = Decoded {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 128, 0, 255, 0, 255],
            source: ColorType::Rgba,
        };
        assert_eq!(image.pbrt_scalar(), [55, 255]);
        image.rgba[3] = 255;
        assert_eq!(image.pbrt_scalar(), [85, 85]);
        image.source = ColorType::GrayscaleAlpha;
        image.rgba = vec![128, 128, 128, 0, 255, 255, 255, 0];
        assert_eq!(image.pbrt_scalar(), [55, 255]);
    }

    #[test]
    fn cutout_mips_keep_covered_color_and_the_closest_threshold_count() {
        let pixels = [255, 0, 0, 0, 0, 255, 0, 255];
        assert_eq!(
            downsample(&pixels, 2, 1, ImageRole::Color, true),
            [0, 255, 0, 128]
        );
        let mut level: Vec<_> = [255, 100, 60, 0]
            .into_iter()
            .flat_map(|a| [0, 0, 0, a])
            .collect();
        preserve_coverage(&mut level, 0.5);
        let covered: Vec<_> = level
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[3] >= 128)
            .collect();
        assert_eq!(covered, [true, true, false, false]);
        assert_eq!(level[15], 0);
    }

    #[test]
    fn png_cook_averages_srgb_in_linear_light_and_preserves_linear_alpha() {
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, 2, 1);
        encoder.set_color(ColorType::GrayscaleAlpha);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0, 0, 255, 255]).unwrap();
        writer.finish().unwrap();
        let encoded = cook(&png, ImageRole::Color).unwrap();
        let image = ImageBlob::decode(&encoded).unwrap();
        assert_eq!(image.mip_levels, 2);
        assert!(image.srgb);
        assert_eq!(
            image.pixels,
            [0, 0, 0, 0, 255, 255, 255, 255, 188, 188, 188, 128]
        );
    }

    #[test]
    fn diagonal_normal_is_renormalized_after_filtering() {
        let pixels = [255, 128, 128, 255, 128, 255, 128, 255];
        assert_eq!(
            downsample(&pixels, 2, 1, ImageRole::Normal, false),
            [218, 218, 128, 255]
        );
    }

    #[test]
    fn odd_width_covers_both_edges_and_splits_the_middle_texel() {
        let pixels: Vec<_> = [0, 0, 100, 0, 200]
            .into_iter()
            .flat_map(|v| [v; 4])
            .collect();
        assert_eq!(
            downsample(&pixels, 5, 1, ImageRole::Orm, false),
            [20, 20, 20, 20, 100, 100, 100, 100]
        );
    }
}

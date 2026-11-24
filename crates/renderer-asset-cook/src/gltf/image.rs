use super::ImageRole;
use png::{ColorType, Decoder, Transformations};
use renderer_asset::ImageBlob;
use std::{error::Error, io::Cursor, ops::Range};

pub(super) fn cook(bytes: &[u8], role: ImageRole) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut decoder = Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
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
    let mut blob = ImageBlob {
        width: frame.width,
        height: frame.height,
        mip_levels: 1,
        srgb: matches!(role, ImageRole::Color),
        pixels: rgba,
    };
    let (mut w, mut h) = (width, height);
    let mut start = 0;
    while w > 1 || h > 1 {
        let next = downsample(&blob.pixels[start..], w, h, role);
        start = blob.pixels.len();
        blob.pixels.extend(next);
        blob.mip_levels += 1;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    Ok(blob.encode()?)
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

fn downsample(pixels: &[u8], width: usize, height: usize, role: ImageRole) -> Vec<u8> {
    let w = (width / 2).max(1);
    let h = (height / 2).max(1);
    let mut out = Vec::with_capacity(w * h * 4);
    let to_linear: [f64; 256] = std::array::from_fn(|i| {
        let value = i as f64 / 255.;
        match role {
            ImageRole::Color => srgb_to_linear(value),
            ImageRole::Normal => 2. * value - 1.,
            ImageRole::Orm => value,
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
                    for channel in 0..3 {
                        sum[channel] += weight * to_linear[pixels[offset + channel] as usize];
                    }
                    sum[3] += weight * f64::from(pixels[offset + 3]) / 255.;
                }
            }
            let [r, g, b, alpha] = sum.map(|v| v / (width as f64 * height as f64));
            let rgb = match role {
                ImageRole::Color => [r, g, b].map(linear_to_srgb),
                ImageRole::Orm => [r, g, b],
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
            downsample(&pixels, 2, 1, ImageRole::Normal),
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
            downsample(&pixels, 5, 1, ImageRole::Orm),
            [20, 20, 20, 20, 100, 100, 100, 100]
        );
    }
}

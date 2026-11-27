use super::Result;
use glam::{Mat3, Mat4, Vec3};
use sashiko_asset::{ObjectStore, scene::InfiniteSky};

/// A marginal and a conditional CDF over equal-solid-angle cells. Every
/// original texel contributes to its cell, including a sub-cell sun disk.
pub(super) fn append(
    bytes: &mut Vec<u8>,
    store: &ObjectStore,
    sky: &InfiniteSky,
    limit: u64,
) -> Result<()> {
    let map = &sky.equal_area_radiance;
    let size = map.width as usize;
    if size == 0 || map.width != map.height || map.mip_levels != 1 {
        return Err("expected a single square source sky".into());
    }
    let source = store.load(&map.data)?;
    let texels = u32::try_from(bytes.len())?;
    let cdf = u32::try_from(
        bytes
            .len()
            .checked_add(source.len())
            .ok_or("sky size overflow")?,
    )?;
    // Prefer the native texel grid so a narrow sun is sampled with its true
    // probability. If the descriptor cannot fit it, aggregate equal-area cells.
    let available = limit
        .min(u64::from(u32::MAX))
        .saturating_sub(u64::from(cdf));
    let bins = (1..=size)
        .rev()
        .find(|&n| {
            size.is_multiple_of(n)
                && (n as u64 + 1)
                    .checked_pow(2)
                    .is_some_and(|w| w <= available / 4)
        })
        .ok_or("film sky exceeds the storage buffer range")?;
    let transform = Mat3::from_mat4(Mat4::from_cols_array_2d(&sky.world_from_light));
    let rotation = Mat3::from_cols(
        transform.x_axis.normalize(),
        transform.y_axis.normalize(),
        transform.z_axis.normalize(),
    );
    let scale = Vec3::new(
        transform.x_axis.length(),
        transform.y_axis.length(),
        transform.z_axis.length(),
    );
    if !(rotation.transpose() * rotation).abs_diff_eq(Mat3::IDENTITY, 1e-4)
        || scale.max_element() - scale.min_element() > 1e-4 * scale.max_element()
    {
        return Err("importance sampling requires an orthogonal infinite light transform".into());
    }
    bytes.extend_from_slice(&source);
    let marginal = bytes.len();
    bytes.resize(marginal + 4 * (bins + 1), 0);
    let cell_size = size / bins;
    let mut rows = Vec::with_capacity(bins);
    let mut weights = vec![0.; bins];
    for strip in source.chunks_exact(8 * size * cell_size) {
        weights.fill(0.);
        for (i, p) in strip.as_chunks::<8>().0.iter().enumerate() {
            let rgb = Vec3::from_array(std::array::from_fn(|c| {
                half::f16::from_le_bytes([p[2 * c], p[2 * c + 1]]).to_f32()
            }));
            if !rgb.is_finite() {
                return Err("nonfinite film environment radiance".into());
            }
            weights[(i % size) / cell_size] +=
                f64::from(rgb.max(Vec3::ZERO).dot(Vec3::new(0.2126, 0.7152, 0.0722)));
        }
        let start = bytes.len();
        bytes.resize(start + 4 * (bins + 1), 0);
        rows.push(write_cdf(&weights, &mut bytes[start..]));
    }
    write_cdf(&rows, &mut bytes[marginal..marginal + 4 * (bins + 1)]);
    let words = [texels, map.width, cdf, bins as u32, sky.scale.to_bits()];
    for (dst, v) in bytes[..20].as_chunks_mut::<4>().0.iter_mut().zip(words) {
        *dst = v.to_le_bytes();
    }
    for (start, matrix) in [(32, rotation.transpose()), (80, rotation)] {
        for (i, row) in matrix.transpose().to_cols_array_2d().iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let at = start + i * 16 + j * 4;
                bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
    Ok(())
}

fn write_cdf(weights: &[f64], bytes: &mut [u8]) -> f64 {
    let total: f64 = weights.iter().sum();
    let mut sum = 0.;
    for (i, (&weight, edge)) in weights
        .iter()
        .zip(bytes[4..].as_chunks_mut::<4>().0)
        .enumerate()
    {
        sum += weight;
        let p = if total > 0. {
            sum / total
        } else {
            (i + 1) as f64 / weights.len() as f64
        };
        *edge = (p as f32).to_le_bytes();
    }
    total
}

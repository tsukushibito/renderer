//! Convolve the authored equal-solid-angle map for the standard PBR renderer.
use super::{Result, object};
use exr::prelude::f16;
use glam::{DVec3, Mat3, Mat4, Vec3};
use sashiko_asset::{
    BlobRef, ObjectStore,
    scene::{Environment, EnvironmentImage, InfiniteSky, SkyRadiance},
};
use std::{
    f32::consts::{PI, TAU},
    path::Path,
};

struct Cell {
    direction: Vec3,
    energy: Vec3,
    solid_angle: f32,
}

fn equal_area_direction(u: f32, v: f32) -> Vec3 {
    let u = 2. * u - 1.;
    let v = 2. * v - 1.;
    let diagonal = 1. - u.abs() - v.abs();
    let r = 1. - diagonal.abs();
    let phi = if r == 0. {
        0.
    } else {
        std::f32::consts::FRAC_PI_4 * ((v.abs() - u.abs()) / r + 1.)
    };
    let radial = r * (2. - r * r).max(0.).sqrt();
    Vec3::new(
        radial * phi.cos().copysign(u),
        radial * phi.sin().copysign(v),
        (1. - r * r).copysign(diagonal),
    )
}

fn panorama(u: f32, v: f32) -> Vec3 {
    let phi = TAU * (u - 0.5);
    let theta = PI * v;
    Vec3::new(
        theta.sin() * phi.cos(),
        theta.cos(),
        theta.sin() * phi.sin(),
    )
}

fn push(out: &mut Vec<u8>, rgb: Vec3) -> Result<()> {
    for v in [rgb.x, rgb.y, rgb.z, 1.] {
        let encoded = f16::from_f32(v);
        if !encoded.is_finite() {
            return Err("environment exceeds finite RGBA16F range".into());
        }
        out.extend(encoded.to_le_bytes());
    }
    Ok(())
}

fn map(
    directory: &Path,
    w: u32,
    h: u32,
    levels: u32,
    bytes: &[u8],
) -> Result<EnvironmentImage<BlobRef>> {
    Ok(EnvironmentImage {
        width: w,
        height: h,
        mip_levels: levels,
        data: object(directory, bytes)?,
    })
}

// Sum every original texel. A small bright solar disk must retain its energy
// even when no quadrature cell centre falls inside the disk.
fn cells(store: &ObjectStore, sky: &InfiniteSky) -> Result<Vec<Cell>> {
    let map = &sky.equal_area_radiance;
    let source = store.load(&map.data)?;
    let size = map.width as usize;
    let bins = size.min(64);
    let rotation = Mat3::from_mat4(Mat4::from_cols_array_2d(&sky.world_from_light));
    let area = 4. * PI / (size as f32).powi(2);
    let mut result = Vec::with_capacity(bins * bins);
    for y in 0..bins {
        let (y0, y1) = (y * size / bins, (y + 1) * size / bins);
        for x in 0..bins {
            let (x0, x1) = (x * size / bins, (x + 1) * size / bins);
            let mut sum = DVec3::ZERO;
            let mut moment = glam::DVec2::ZERO;
            let luminance = DVec3::new(0.2126, 0.7152, 0.0722);
            for row in y0..y1 {
                for (dx, p) in source[8 * (row * size + x0)..8 * (row * size + x1)]
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .enumerate()
                {
                    let channel = |i: usize| f16::from_le_bytes([p[i], p[i + 1]]).to_f64();
                    let rgb = DVec3::new(channel(0), channel(2), channel(4));
                    sum += rgb;
                    moment += glam::DVec2::new((x0 + dx) as f64 + 0.5, row as f64 + 0.5)
                        * rgb.dot(luminance);
                }
            }
            // Subdivide small maps so one source texel still covers its full
            // spherical footprint when integrating a hemisphere.
            let split = 64_usize.div_ceil(size);
            let subarea = area / (split * split) as f32;
            let uv = |lo: usize, hi: usize, i: usize| {
                (lo as f32 + (hi - lo) as f32 * (i as f32 + 0.5) / split as f32) / size as f32
            };
            for sy in 0..split {
                for sx in 0..split {
                    let centre = if split == 1 && sum.dot(luminance) > 0. {
                        // A solar disk near a bin edge must retain its direction as
                        // well as its integrated flux.
                        (moment / (sum.dot(luminance) * size as f64)).as_vec2()
                    } else {
                        glam::Vec2::new(uv(x0, x1, sx), uv(y0, y1, sy))
                    };
                    let direction = rotation * equal_area_direction(centre.x, centre.y);
                    result.push(Cell {
                        direction: direction.normalize(),
                        energy: sum.as_vec3() * subarea,
                        solid_angle: ((x1 - x0) * (y1 - y0)) as f32 * subarea,
                    });
                }
            }
        }
    }
    Ok(result)
}

fn convolution(cells: &[Cell], n: Vec3, roughness: Option<f32>) -> Vec3 {
    let mut sum = Vec3::ZERO;
    let mut weight = 0.;
    let a2 = roughness.unwrap_or(1.).powi(4);
    for cell in cells {
        let cosine = n.dot(cell.direction);
        if cosine > 0. {
            // For V=N, the split-sum reflection integral weights radiance by
            // NoL * D(NoH); NoH^2 = (1 + NoL)/2.
            let shape = if roughness.is_some() {
                1. / (0.5 * (1. + cosine) * (a2 - 1.) + 1.).powi(2)
            } else {
                1.
            };
            let w = cosine * shape;
            sum += cell.energy * w;
            weight += cell.solid_angle * w;
        }
    }
    if weight == 0. {
        return Vec3::ZERO;
    }
    sum / weight * if roughness.is_some() { 1. } else { PI }
}

fn brdf_lut(directory: &Path) -> Result<EnvironmentImage<BlobRef>> {
    const SIZE: u32 = 128;
    const SAMPLES: u32 = 512;
    let mut bytes = Vec::with_capacity((8 * SIZE * SIZE) as usize);
    for y in 0..SIZE {
        let roughness = (y as f32 + 0.5) / SIZE as f32;
        let a2 = roughness.powi(2).max(0.001).powi(2);
        let mut halves = Vec::with_capacity(SAMPLES as usize);
        for i in 0..SAMPLES {
            let xi = i.reverse_bits() as f32 / 4294967296.;
            let z = ((1. - xi) / (1. + (a2 - 1.) * xi)).sqrt();
            halves.push(Vec3::new(
                (1. - z * z).max(0.).sqrt() * (TAU * i as f32 / SAMPLES as f32).cos(),
                0.,
                z,
            ));
        }
        for x in 0..SIZE {
            let nv = (x as f32 + 0.5) / SIZE as f32;
            let v = Vec3::new((1. - nv * nv).sqrt(), 0., nv);
            let mut value = Vec3::ZERO;
            for h in &halves {
                let vh = v.dot(*h).max(0.);
                let nl = 2. * vh * h.z - nv;
                if nl > 0. {
                    let smith = nv * (a2 + (1. - a2) * nl * nl).sqrt()
                        + nl * (a2 + (1. - a2) * nv * nv).sqrt();
                    let visibility = 2. * nl * vh / (h.z * smith);
                    let f = (1. - vh).powi(5);
                    value += visibility * Vec3::new(1. - f, f, 0.);
                }
            }
            push(&mut bytes, value / SAMPLES as f32)?;
        }
    }
    map(directory, SIZE, SIZE, 1, &bytes)
}

pub(super) fn cook(directory: &Path, sky: &InfiniteSky) -> Result<Environment<BlobRef>> {
    let store = ObjectStore::new(directory);
    let cells = cells(&store, sky)?;
    let mut bytes = Vec::new();
    for y in 0..64 {
        for x in 0..128 {
            let normal = panorama((x as f32 + 0.5) / 128., (y as f32 + 0.5) / 64.);
            push(&mut bytes, convolution(&cells, normal, None))?;
        }
    }
    let diffuse = map(directory, 128, 64, 1, &bytes)?;
    // Keep a directional 32x16 tail at roughness one.
    let source = SkyRadiance::open(&store, sky)?;
    bytes.clear();
    for level in 0..=4 {
        let (w, h) = (512 >> level, 256 >> level);
        for y in 0..h {
            for x in 0..w {
                let direction =
                    |dx, dy| panorama((x as f32 + dx) / w as f32, (y as f32 + dy) / h as f32);
                let radiance = if level == 0 {
                    let mut sum = Vec3::ZERO;
                    if sky.scale > 0. {
                        for v in 0..8 {
                            for u in 0..8 {
                                sum += source.sample(direction(
                                    (u as f32 + 0.5) / 8.,
                                    (v as f32 + 0.5) / 8.,
                                )) / sky.scale;
                            }
                        }
                    }
                    sum / 64.
                } else {
                    convolution(&cells, direction(0.5, 0.5), Some(level as f32 / 4.))
                };
                push(&mut bytes, radiance)?;
            }
        }
    }
    let reflection = map(directory, 512, 256, 5, &bytes)?;
    Ok(Environment {
        maps: [diffuse, reflection, brdf_lut(directory)?],
        nits_per_unit: sky.scale,
        yaw_radians: 0.,
    })
}

//! View-local data for indirect diffuse shadows on static surface pages.
use super::{BakedLighting, PreparedView, VIEW_BYTES};
use sashiko_renderer_core::graph::{BufferDesc, BufferHandle, Graph, GraphError};

/// Shadow proxy with world-space endpoints. Coincident endpoints define a sphere.
#[derive(Clone, Copy, Debug)]
pub struct ShadowCapsule {
    a: [f32; 3],
    b: [f32; 3],
    radius: f32,
}

impl ShadowCapsule {
    pub fn new(a: [f32; 3], b: [f32; 3], radius: f32) -> Result<Self, GraphError> {
        if !radius.is_finite() || radius <= 0.0 || a.into_iter().chain(b).any(|v| !v.is_finite()) {
            return Err(GraphError::invalid_input("invalid indirect shadow capsule"));
        }
        Ok(Self { a, b, radius })
    }
}

pub const MAX_INDIRECT_SHADOW_CAPSULES: usize = 16;
const GRID: usize = 16;
const MASK_OFFSET: usize = VIEW_BYTES + 16;
const RECORD_OFFSET: usize = MASK_OFFSET + GRID * GRID * 4;
pub(crate) const BYTES: usize = RECORD_OFFSET + MAX_INDIRECT_SHADOW_CAPSULES * 48;
pub(crate) const DESC: BufferDesc = BufferDesc { size: BYTES as u64 };

/// Sample the constant and linear luminance coefficients of the *irradiance*
/// polynomial. Visibility, relocation and scenario interpolation follow probe_volume.hlsli.
fn direction(volume: &BakedLighting, center: [f32; 3], blend: f32) -> Option<[f32; 4]> {
    let texels = volume.rgba32f().as_chunks::<16>().0;
    let texel = |at: usize| -> [f32; 4] {
        std::array::from_fn(|j| {
            f32::from_le_bytes(texels[at][4 * j..4 * j + 4].try_into().unwrap())
        })
    };
    let bounds = texel(0);
    let mut size = bounds[3];
    let mut f: [f32; 3] = std::array::from_fn(|j| (center[j] - bounds[j]) / size);
    if !f.iter().all(|x| (0.0..=1.0).contains(x)) {
        return None;
    }
    let count = volume.scenario_count();
    let header = 2 + count.max(2);
    let base = header + texel(1)[0] as usize * 3;
    let coeffs = volume.sh_order().coefficient_count();
    let stride = 2 + count * coeffs;
    let phase = blend * (count - 1) as f32;
    let key = phase as usize;
    let mut node = 0;
    loop {
        let at = header + node * 3;
        let first = texel(at)[0] as usize;
        if first == 0 {
            let mut sh = [0.0f64; 4];
            for i in 0..8 {
                let id = texel(at + 1 + i / 4)[i % 4] as usize;
                let p = base + id * stride;
                let minus = texel(p);
                let plus = texel(p + 1);
                let mut weight = minus[3];
                let mut visibility = 1.0f32;
                for j in 0..3 {
                    let side = ((i >> j) & 1) as f32;
                    weight *= if side == 0.0 { 1.0 - f[j] } else { f[j] };
                    let delta = (f[j] - side) * size - texel(p + 2 + j)[3];
                    let wall = if delta >= 0.0 { plus[j] } else { minus[j] };
                    visibility = visibility.min(
                        (1.0 + (wall - delta.abs()) / (size * 0.05).max(1e-6)).clamp(0.0, 1.0),
                    );
                }
                for (k, sum) in sh.iter_mut().enumerate() {
                    let luminance = |scenario| {
                        let c = texel(p + 2 + coeffs * scenario + k);
                        0.2126 * f64::from(c[0])
                            + 0.7152 * f64::from(c[1])
                            + 0.0722 * f64::from(c[2])
                    };
                    let a = luminance(key);
                    let b = luminance((key + 1).min(count - 1));
                    *sum +=
                        (a + (b - a) * f64::from(phase.fract())) * f64::from(weight * visibility);
                }
            }
            if sh[0] <= 0.0 {
                return None;
            }
            let length = sh[1].hypot(sh[2]).hypot(sh[3]);
            if length <= f64::EPSILON * sh[0] {
                return Some([0.; 4]);
            }
            return Some([
                (sh[1] / length) as f32,
                (sh[2] / length) as f32,
                (sh[3] / length) as f32,
                (length / sh[0]).min(1.0) as f32,
            ]);
        }
        node = first;
        for (j, coordinate) in f.iter_mut().enumerate() {
            let high = usize::from(*coordinate >= 0.5);
            node += high << j;
            *coordinate = (2.0 * *coordinate - high as f32).clamp(0.0, 1.0);
        }
        size *= 0.5;
    }
}

// Project a conservative world AABB. Crossing the eye plane covers all tiles.
fn tile_bounds(c: ShadowCapsule, matrix: &[[f32; 4]; 4]) -> Option<[[usize; 2]; 2]> {
    let mut lower = [f64::INFINITY; 2];
    let mut upper = [f64::NEG_INFINITY; 2];
    for corner in 0..8 {
        let point: [f64; 3] = std::array::from_fn(|j| {
            let r = 6.0 * f64::from(c.radius);
            if corner & (1 << j) == 0 {
                f64::from(c.a[j].min(c.b[j])) - r
            } else {
                f64::from(c.a[j].max(c.b[j])) + r
            }
        });
        let clip = |row: usize| {
            f64::from(matrix[3][row])
                + (0..3)
                    .map(|j| point[j] * f64::from(matrix[j][row]))
                    .sum::<f64>()
        };
        let w = clip(3);
        if w <= 1e-8 {
            return Some([[0, 0], [GRID - 1, GRID - 1]]);
        }
        for j in 0..2 {
            let v = (clip(j) / w + 1.0) * (GRID as f64 / 2.0);
            lower[j] = lower[j].min(v - 1e-4);
            upper[j] = upper[j].max(v + 1e-4);
        }
    }
    if (0..2).any(|j| upper[j] < 0.0 || lower[j] >= GRID as f64) {
        return None;
    }
    Some([lower, upper].map(|xy| xy.map(|v| (v as usize).min(GRID - 1))))
}

pub(crate) fn upload(
    graph: &mut Graph,
    view: &PreparedView,
    volume: &BakedLighting,
    blend: f32,
    capsules: &[ShadowCapsule],
) -> Result<Option<BufferHandle>, GraphError> {
    let mut bytes = [0u8; BYTES];
    let mut masks = [0u32; GRID * GRID];
    for (i, c) in capsules.iter().enumerate() {
        let Some([lo, hi]) = tile_bounds(*c, view.matrix()) else {
            continue;
        };
        let center = std::array::from_fn(|j| 0.5 * c.a[j] + 0.5 * c.b[j]);
        let Some(light) = direction(volume, center, blend) else {
            continue;
        };
        let record =
            c.a.into_iter()
                .chain([c.radius])
                .chain(c.b)
                .chain([0.0])
                .chain(light);
        for (dst, v) in bytes[RECORD_OFFSET + i * 48..]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(record)
        {
            dst.copy_from_slice(&v.to_le_bytes());
        }
        for y in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                masks[y * GRID + x] |= 1 << i;
            }
        }
    }
    if masks.iter().all(|m| *m == 0) {
        return Ok(None);
    }
    bytes[..VIEW_BYTES].copy_from_slice(view.uniform_bytes());
    for (dst, pixels) in bytes[VIEW_BYTES..]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(view.extent)
    {
        dst.copy_from_slice(&(GRID as f32 / pixels as f32).to_le_bytes());
    }
    for (dst, mask) in bytes[MASK_OFFSET..]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(masks)
    {
        dst.copy_from_slice(&mask.to_le_bytes());
    }
    let buffer = graph.buffer("indirect shadow view", DESC)?;
    graph.upload_buffer("indirect shadow upload", buffer, 0, &bytes)?;
    Ok(Some(buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposing_scenario_directions_cancel_without_flipping_contact_shadows() {
        let mut bytes = BakedLighting::uniform([[1.; 3]; 2]).unwrap().into_rgba32f();
        for (texel, first) in [(10, 1.0f32), (14, -1.0)] {
            for channel in 0..3 {
                let at = texel * 16 + channel * 4;
                bytes[at..at + 4].copy_from_slice(&first.to_le_bytes());
            }
        }
        let volume = BakedLighting::from_texels(bytes).unwrap();
        assert_eq!(direction(&volume, [0.5; 3], 0.), Some([1., 0., 0., 1.]));
        assert_eq!(direction(&volume, [0.5; 3], 0.5), Some([0.; 4]));
        assert_eq!(direction(&volume, [0.5; 3], 1.), Some([-1., 0., 0., 1.]));
        assert_eq!(direction(&volume, [-0.01; 3], 0.), None);
    }

    #[test]
    fn offscreen_caster_is_retained_when_its_influence_reaches_the_view() {
        let matrix = crate::unlit_forward::IDENTITY;
        let caster = |x| ShadowCapsule::new([x, 0., 0.5], [x, 0.1, 0.5], 0.05).unwrap();
        assert_eq!(tile_bounds(caster(1.15), &matrix).unwrap()[1][0], GRID - 1);
        assert!(tile_bounds(caster(1.5), &matrix).is_none());
    }

    #[test]
    fn shader_camera_block_matches_host_record_and_mask_offsets() {
        let reflection: serde_json::Value = serde_json::from_str(include_str!(
            "../../shaders/dxil/gbuffer_indirect_shadow.frag.spirv-reflection.json"
        ))
        .unwrap();
        let camera = reflection["ubos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ubo| ubo["set"] == 0 && ubo["binding"] == 0)
            .unwrap();
        assert_eq!(camera["block_size"], BYTES);
        let members = reflection["types"][camera["type"].as_str().unwrap()]["members"]
            .as_array()
            .unwrap();
        for (name, offset) in [
            ("shadow_tile_scale", VIEW_BYTES),
            ("shadow_masks", MASK_OFFSET),
            ("capsule_data", RECORD_OFFSET),
        ] {
            assert_eq!(
                members.iter().find(|m| m["name"] == name).unwrap()["offset"],
                offset
            );
        }
    }
}

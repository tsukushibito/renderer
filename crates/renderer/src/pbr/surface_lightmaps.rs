//! Immutable, independently uploadable surface lighting. One address identifies
//! the baked lighting of a placement, including both sides of a thin surface.
use super::BakedLighting;
use renderer_core::graph::{GraphError, ImageDesc};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightmapAddress {
    pub page: usize,
    pub slot: u32,
}
pub struct LightmapPage {
    extent: [u32; 2],
    texels: Vec<u8>,
}
impl LightmapPage {
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
    pub fn image_desc(&self) -> ImageDesc {
        ImageDesc::rgba32_float(self.extent[0], self.extent[1])
    }
    pub fn rgba32f(&self) -> &[u8] {
        &self.texels
    }
    pub fn into_rgba32f(self) -> Vec<u8> {
        self.texels
    }
}
pub struct SurfaceLightmaps {
    pub(crate) pages: Vec<LightmapPage>,
    pub(crate) placements: BTreeMap<u32, LightmapAddress>,
    pub(crate) probes: BakedLighting,
}
fn invalid() -> GraphError {
    GraphError::invalid_input(
        "invalid static lightmap file; recook the geometry and baked lighting",
    )
}
fn take<'a>(data: &mut &'a [u8], n: usize) -> Result<&'a [u8], GraphError> {
    let (a, b) = data.split_at_checked(n).ok_or_else(invalid)?;
    *data = b;
    Ok(a)
}
fn word(data: &mut &[u8]) -> Result<u32, GraphError> {
    Ok(u32::from_le_bytes(take(data, 4)?.try_into().unwrap()))
}

impl SurfaceLightmaps {
    pub fn pages(&self) -> &[LightmapPage] {
        &self.pages
    }
    pub fn placements(&self) -> &BTreeMap<u32, LightmapAddress> {
        &self.placements
    }
    pub fn probes(&self) -> &BakedLighting {
        &self.probes
    }
    pub fn into_parts(
        self,
    ) -> (
        Vec<LightmapPage>,
        BTreeMap<u32, LightmapAddress>,
        BakedLighting,
    ) {
        (self.pages, self.placements, self.probes)
    }

    pub const MAGIC: &[u8; 8] = b"RSURF002";
    /// Import at the asset boundary. Each GPU image includes the lookup table
    /// of the placements sharing the same front/back page pair.
    pub fn from_bytes(mut data: &[u8]) -> Result<Self, GraphError> {
        if take(&mut data, 8)? != Self::MAGIC {
            return Err(invalid());
        }
        let keys = word(&mut data)? as usize;
        let count = word(&mut data)? as usize;
        let n = word(&mut data)? as usize;
        if !(1..=BakedLighting::MAX_SCENARIOS).contains(&keys) || count > data.len() / 8 {
            return Err(invalid());
        }
        let mut extents = Vec::with_capacity(count);
        for _ in 0..count {
            let e = [word(&mut data)?, word(&mut data)?];
            if e.iter().any(|v| !(1..=2048).contains(v)) {
                return Err(invalid());
            }
            extents.push(e);
        }
        if n > data.len() / 28 {
            return Err(invalid());
        }
        #[derive(Clone, Copy)]
        struct Rect {
            page: usize,
            start: u32,
            extent: [u32; 2],
        }
        let mut entries = BTreeMap::<u32, [Option<Rect>; 2]>::new();
        for _ in 0..n {
            let id = word(&mut data)?;
            let page = word(&mut data)? as usize;
            let side = word(&mut data)? as usize;
            let origin = [word(&mut data)?, word(&mut data)?];
            let extent = [word(&mut data)?, word(&mut data)?];
            let dims = *extents.get(page).ok_or_else(invalid)?;
            if side > 1
                || (0..2).any(|i| {
                    extent[i] == 0
                        || origin[i] == 0
                        || origin[i]
                            .checked_add(extent[i])
                            .is_none_or(|v| v >= dims[i])
                })
            {
                return Err(invalid());
            }
            let start = origin[1] * dims[0] + origin[0];
            if entries.entry(id).or_default()[side]
                .replace(Rect {
                    page,
                    start,
                    extent,
                })
                .is_some()
            {
                return Err(invalid());
            }
        }
        let mut raw = Vec::new();
        for &[w, h] in &extents {
            let rgb = take(&mut data, (w * h) as usize * keys * 12)?;
            if rgb.as_chunks::<4>().0.iter().any(|v| {
                let v = f32::from_le_bytes(*v);
                !v.is_finite() || v < 0.
            }) {
                return Err(invalid());
            }
            raw.push(rgb);
        }
        let probes = BakedLighting::from_bytes(data)?;
        if probes.scenario_count() != keys {
            return Err(invalid());
        }
        let mut groups = BTreeMap::<(usize, usize), Vec<_>>::new();
        for (id, [front, back]) in entries {
            let f = front.ok_or_else(invalid)?;
            let b = back.unwrap_or(f);
            if f.extent != b.extent {
                return Err(invalid());
            }
            groups.entry((f.page, b.page)).or_default().push((id, f, b));
        }
        // Every physical page is uploaded exactly once, even for a two-page placement.
        let mut assigned = vec![false; count];
        for &(front, back) in groups.keys() {
            if assigned[front] || assigned[back] {
                return Err(invalid());
            }
            assigned[front] = true;
            assigned[back] = true;
        }
        if assigned.contains(&false) {
            return Err(invalid());
        }
        let mut pages = Vec::new();
        let mut placements = BTreeMap::new();
        for ((front, back), members) in groups {
            let header = 1 + members.len() * 2;
            let front_len = raw[front].len() / 12;
            let back_offset = if back == front { 0 } else { front_len };
            let size = header
                + front_len
                + if back == front {
                    0
                } else {
                    raw[back].len() / 12
                };
            if header > 1 << 24 {
                return Err(invalid());
            }
            let width = size.min((size.isqrt() + 1).next_power_of_two().clamp(1, 16384)) as u32;
            let height = size.div_ceil(width as usize) as u32;
            if height > 16384 {
                return Err(invalid());
            }
            let mut texels = Vec::with_capacity(width as usize * height as usize * 16);
            // Metadata is encoded as finite numeric words. Split the page base
            // from the scenario stride so offsets remain exact in float32.
            let mut push = |v: [u32; 4]| {
                texels.extend(v.into_iter().map(|v| v as f32).flat_map(f32::to_le_bytes))
            };
            push([members.len() as u32, keys as u32, header as u32, 0]);
            for (index, (id, f, b)) in members.into_iter().enumerate() {
                placements.insert(
                    id,
                    LightmapAddress {
                        page: pages.len(),
                        slot: index as u32 + 1,
                    },
                );
                push([
                    f.start,
                    extents[front][0],
                    b.start + (back_offset / keys) as u32,
                    extents[back][0],
                ]);
                push([f.extent[0], f.extent[1], 0, 0]);
            }
            for bytes in std::iter::once(raw[front]).chain((front != back).then_some(raw[back])) {
                for rgb in bytes.as_chunks::<12>().0 {
                    texels.extend_from_slice(rgb);
                    texels.extend_from_slice(&[0; 4]);
                }
            }
            texels.resize(width as usize * height as usize * 16, 0);
            pages.push(LightmapPage {
                extent: [width, height],
                texels,
            });
        }
        Ok(Self {
            pages,
            placements,
            probes,
        })
    }
}

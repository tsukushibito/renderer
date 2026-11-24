//! Portable layout index and independently addressable, GPU-ready lightmap pages.
use super::BakedLighting;
use renderer_core::graph::{GraphError, ImageDesc};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightmapAddress {
    /// Dense index in the loaded selection.
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
    pub(crate) complete: bool,
    pub(crate) pages: Vec<LightmapPage>,
    pub(crate) placements: BTreeMap<u32, LightmapAddress>,
    pub(crate) probes: Option<BakedLighting>,
}
impl SurfaceLightmaps {
    pub fn pages(&self) -> &[LightmapPage] {
        &self.pages
    }
    pub fn placements(&self) -> &BTreeMap<u32, LightmapAddress> {
        &self.placements
    }
    pub fn probes(&self) -> Option<&BakedLighting> {
        self.probes.as_ref()
    }
    pub fn into_parts(
        self,
    ) -> (
        Vec<LightmapPage>,
        BTreeMap<u32, LightmapAddress>,
        Option<BakedLighting>,
    ) {
        (self.pages, self.placements, self.probes)
    }
}

fn invalid() -> GraphError {
    GraphError::invalid_input("invalid surface lighting index or page; recook the surface lighting")
}
fn take<'a>(data: &mut &'a [u8], n: usize) -> Result<&'a [u8], GraphError> {
    let (a, b) = data.split_at_checked(n).ok_or_else(invalid)?;
    *data = b;
    Ok(a)
}
fn word(data: &mut &[u8]) -> Result<u32, GraphError> {
    Ok(u32::from_le_bytes(take(data, 4)?.try_into().unwrap()))
}

/// Describes one complete GPU image, including the placement lookup table in its first texels.
#[derive(Clone)]
pub struct SurfacePageRecord {
    extent: [u32; 2],
    scenarios: usize,
    placements: Vec<u32>,
    sha256: [u8; 32],
}
impl SurfacePageRecord {
    /// Cook boundary. IDs are in shader slot order (the first ID uses slot 1).
    pub fn from_texels(
        scenarios: usize,
        extent: [u32; 2],
        placements: Vec<u32>,
        rgba32f: &[u8],
    ) -> Result<Self, GraphError> {
        let record = Self {
            extent,
            scenarios,
            placements,
            sha256: Sha256::digest(rgba32f).into(),
        };
        record.check_payload(rgba32f)?;
        Ok(record)
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
    pub fn placements(&self) -> &[u32] {
        &self.placements
    }
    fn pixel_count(&self) -> Result<usize, GraphError> {
        let [w, h] = self.extent;
        if !(1..=16384).contains(&w)
            || !(1..=16384).contains(&h)
            || !(1..=BakedLighting::MAX_SCENARIOS).contains(&self.scenarios)
            || self.placements.is_empty()
            || self.placements.len() > ((1 << 24) - 1) / 2
        {
            return Err(invalid());
        }
        let count = (w * h) as usize;
        if self.placements.len() * 2 + 1 >= count {
            return Err(invalid());
        }
        Ok(count)
    }
    fn check_payload(&self, bytes: &[u8]) -> Result<(), GraphError> {
        let count = self.pixel_count()?;
        if count.checked_mul(16) != Some(bytes.len()) {
            return Err(invalid());
        }
        let integer = |index: usize| -> Result<usize, GraphError> {
            let f = f32::from_le_bytes(bytes[index * 4..][..4].try_into().unwrap());
            if !(0.0..=16777216.).contains(&f) || f.fract() != 0. {
                return Err(invalid());
            }
            Ok(f as usize)
        };
        let start = self.placements.len() * 2 + 1;
        if integer(0)? != self.placements.len()
            || integer(1)? != self.scenarios
            || integer(2)? != start
            || integer(3)? != 0
        {
            return Err(invalid());
        }
        let pixels = (count - start) / self.scenarios;
        for i in 0..self.placements.len() {
            let base = 4 + i * 8;
            let w = integer(base + 4)?;
            let h = integer(base + 5)?;
            if !(1..=2048).contains(&w) || !(1..=2048).contains(&h) {
                return Err(invalid());
            }
            for side in [base, base + 2] {
                let origin = integer(side)?;
                let pitch = integer(side + 1)?;
                // Bilinear filtering reads a one-texel border at UV 0 and 1.
                if pitch < w + 2
                    || pitch > 2048
                    || origin < pitch + 1
                    || origin + w + h * pitch >= pixels
                {
                    return Err(invalid());
                }
            }
        }
        if bytes[start * 16..].as_chunks::<4>().0.iter().any(|v| {
            let value = f32::from_le_bytes(*v);
            !value.is_finite() || value < 0.
        }) {
            return Err(invalid());
        }
        Ok(())
    }
}

/// A frozen generation. Content hashes pin each independently loaded object, including probes.
pub struct SurfaceLightmapIndex {
    scenarios: usize,
    pages: Vec<SurfacePageRecord>,
    probe_sha256: [u8; 32],
}
impl SurfaceLightmapIndex {
    pub const MAGIC: &[u8; 8] = b"RSURF003";
    pub fn new(
        scenarios: usize,
        pages: Vec<SurfacePageRecord>,
        probe_sha256: [u8; 32],
    ) -> Result<Self, GraphError> {
        if !(1..=BakedLighting::MAX_SCENARIOS).contains(&scenarios)
            || pages.len() > u32::MAX as usize
        {
            return Err(invalid());
        }
        let mut ids = BTreeSet::new();
        for record in &pages {
            record.pixel_count()?;
            if record.scenarios != scenarios || record.placements.iter().any(|id| !ids.insert(*id))
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            scenarios,
            pages,
            probe_sha256,
        })
    }
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
    pub fn pages(&self) -> &[SurfacePageRecord] {
        &self.pages
    }
    pub fn scenario_count(&self) -> usize {
        self.scenarios
    }
    pub fn probe_sha256(&self) -> &[u8; 32] {
        &self.probe_sha256
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Self::MAGIC.to_vec();
        bytes.extend((self.scenarios as u32).to_le_bytes());
        bytes.extend((self.pages.len() as u32).to_le_bytes());
        bytes.extend(self.probe_sha256);
        for record in &self.pages {
            for v in [
                record.extent[0],
                record.extent[1],
                record.placements.len() as u32,
            ] {
                bytes.extend(v.to_le_bytes());
            }
            bytes.extend(record.sha256);
            for id in &record.placements {
                bytes.extend(id.to_le_bytes());
            }
        }
        bytes
    }
    pub fn from_bytes(mut data: &[u8]) -> Result<Self, GraphError> {
        if take(&mut data, 8)? != Self::MAGIC {
            return Err(invalid());
        }
        let scenarios = word(&mut data)? as usize;
        let count = word(&mut data)? as usize;
        let probe_sha256 = take(&mut data, 32)?.try_into().unwrap();
        if count > data.len() / 44 {
            return Err(invalid());
        }
        let mut pages = Vec::with_capacity(count);
        for _ in 0..count {
            let extent = [word(&mut data)?, word(&mut data)?];
            let members = word(&mut data)? as usize;
            let sha256 = take(&mut data, 32)?.try_into().unwrap();
            if members > data.len() / 4 {
                return Err(invalid());
            }
            let placements = (0..members)
                .map(|_| word(&mut data))
                .collect::<Result<_, _>>()?;
            pages.push(SurfacePageRecord {
                extent,
                scenarios,
                placements,
                sha256,
            });
        }
        if !data.is_empty() {
            return Err(invalid());
        }
        Self::new(scenarios, pages, probe_sha256)
    }
    /// Load and validate only the requested pages. Their original payload allocations
    /// become the upload inputs. Pass probe bytes only when this selection needs diffuse probes.
    pub fn load(
        &self,
        selected: impl IntoIterator<Item = usize>,
        probe_bytes: Option<&[u8]>,
        mut read: impl FnMut(&[u8; 32]) -> Result<Vec<u8>, Box<dyn Error>>,
    ) -> Result<SurfaceLightmaps, Box<dyn Error>> {
        let probes = probe_bytes
            .map(|bytes| -> Result<_, GraphError> {
                if <[u8; 32]>::from(Sha256::digest(bytes)) != self.probe_sha256 {
                    return Err(invalid());
                }
                let probes = BakedLighting::from_bytes(bytes)?;
                if probes.scenario_count() != self.scenarios {
                    return Err(invalid());
                }
                Ok(probes)
            })
            .transpose()?;
        let mut pages = Vec::new();
        let mut placements = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for id in selected {
            let record = self.pages.get(id).ok_or_else(invalid)?;
            if !seen.insert(id) {
                return Err(invalid().into());
            }
            let texels = read(&record.sha256)?;
            if <[u8; 32]>::from(Sha256::digest(&texels)) != record.sha256 {
                return Err(invalid().into());
            }
            record.check_payload(&texels)?;
            for (i, placement) in record.placements.iter().enumerate() {
                placements.insert(
                    *placement,
                    LightmapAddress {
                        page: pages.len(),
                        slot: i as u32 + 1,
                    },
                );
            }
            pages.push(LightmapPage {
                extent: record.extent,
                texels,
            });
        }
        Ok(SurfaceLightmaps {
            complete: pages.len() == self.pages.len(),
            pages,
            placements,
            probes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page(id: u32, color: f32) -> (SurfacePageRecord, Vec<u8>) {
        let mut words = vec![1., 1., 3., 0., 5., 4., 5., 4., 2., 2., 0., 0.];
        words.resize(20 * 4, color);
        let bytes: Vec<_> = words.into_iter().flat_map(f32::to_le_bytes).collect();
        (
            SurfacePageRecord::from_texels(1, [5, 4], vec![id], &bytes).unwrap(),
            bytes,
        )
    }

    #[test]
    fn frozen_index_reads_only_requested_images_and_retains_their_allocation() {
        let (first, first_bytes) = page(7, 1.);
        let (second, second_bytes) = page(9, 2.);
        let second_hash = *second.sha256();
        let index = SurfaceLightmapIndex::new(1, vec![first, second], [0; 32]).unwrap();
        let index = SurfaceLightmapIndex::from_bytes(&index.to_bytes()).unwrap();
        let pointer = second_bytes.as_ptr();
        let mut payload = Some(second_bytes);
        let partial = index
            .load([1], None, |h| {
                assert_eq!(*h, second_hash);
                Ok(payload.take().unwrap())
            })
            .unwrap();
        assert!(payload.is_none());
        assert_eq!(partial.pages()[0].rgba32f().as_ptr(), pointer);
        assert_eq!(
            *partial.placements().get(&9).unwrap(),
            LightmapAddress { page: 0, slot: 1 }
        );
        assert!(!partial.placements().contains_key(&7));
        assert!(index.load([1], None, |_| Ok(first_bytes.clone())).is_err());
        assert!(
            index
                .load([], Some(&first_bytes), |_| panic!("no pages requested"))
                .is_err()
        );
        assert!(
            index
                .load([2], None, |_| panic!("invalid page ID"))
                .is_err()
        );
    }

    #[test]
    fn reject_duplicate_placements_and_invalid_fetch_ranges_at_import() {
        let (record, mut bytes) = page(4, 1.);
        assert!(SurfaceLightmapIndex::new(1, vec![record.clone(), record], [0; 32]).is_err());
        bytes[16..20].copy_from_slice(&1000.0_f32.to_le_bytes());
        assert!(SurfacePageRecord::from_texels(1, [5, 4], vec![4], &bytes).is_err());
        assert!(SurfaceLightmapIndex::from_bytes(b"RSURF002").is_err());
    }
}

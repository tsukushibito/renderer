//! Cooked spherical harmonic irradiance. Lighting scenarios share one
//! world-space octree, eight corner probes per leaf and visibility data.
use sashiko_renderer_core::graph::GraphError;

/// Highest spherical harmonic degree stored for every probe in a volume.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProbeShOrder {
    #[default]
    L1,
    L2,
}

impl ProbeShOrder {
    /// Number of RGB irradiance coefficients per probe and lighting scenario.
    pub const fn coefficient_count(self) -> usize {
        match self {
            Self::L1 => 4,
            Self::L2 => 9,
        }
    }

    /// GPU atlas header tag for a scenario count in `1..=BakedLighting::MAX_SCENARIOS`.
    pub const fn atlas_tag(self, scenario_count: usize) -> f32 {
        match (self, scenario_count) {
            (Self::L1, 2) => 0.,
            (Self::L1, count) => count as f32,
            (Self::L2, count) => -(count as f32),
        }
    }

    fn from_atlas_tag(tag: f32) -> Option<(Self, usize)> {
        if tag == 0. {
            return Some((Self::L1, 2));
        }
        let count = tag.abs();
        if !(1.0..=BakedLighting::MAX_SCENARIOS as f32).contains(&count)
            || count.fract() != 0.
            || tag == 2.
        {
            return None;
        }
        Some((if tag < 0. { Self::L2 } else { Self::L1 }, count as usize))
    }

    fn file_magic(self, scenarios: usize) -> [u8; 8] {
        match (self, scenarios) {
            (Self::L1, 1) => BakedLighting::SINGLE_MAGIC,
            (Self::L1, 2) => BakedLighting::MAGIC,
            (Self::L1, _) => BakedLighting::SEQUENCE_MAGIC,
            (Self::L2, _) => *b"RBKLSH02",
        }
    }
}

/// Interpretation of the environment's diffuse image.
#[derive(Clone, Copy, Debug, Default)]
pub enum DiffuseLighting {
    #[default]
    EnvironmentMap,
    /// Static placements use lightmap pages. Other draws retain environment diffuse.
    SurfacePages { blend: f32 },
    /// Upload the diffuse texture from a validated BakedLighting. Zero selects
    /// the first baked scenario and one selects the last. Intermediate scenarios are evenly spaced. A single scenario
    /// supplies the same lighting for every blend value.
    BakedLighting { blend: f32, view: BakedLightingView },
}
/// Diagnostic outputs from the same material and spatial lookup as shading.
#[derive(Clone, Copy, Debug, Default)]
pub enum BakedLightingView {
    #[default]
    Shaded,
    Irradiance,
    Sources,
    ProbeWeights,
}
impl DiffuseLighting {
    pub(super) fn uniform(self) -> Result<f32, GraphError> {
        match self {
            Self::EnvironmentMap => Ok(0.),
            Self::SurfacePages { blend } if (0.0..=1.0).contains(&blend) => Ok(-1. - blend),
            Self::BakedLighting { blend, view } if (0.0..=1.0).contains(&blend) => {
                Ok(1. + 2. * (view as u32 as f32) + blend)
            }
            Self::SurfacePages { .. } | Self::BakedLighting { .. } => {
                Err(GraphError::invalid_input("invalid probe scenario blend"))
            }
        }
    }
}

/// Resident-ready linear RGBA32F texture. All structural checks run at import.
pub struct BakedLighting {
    bytes: Vec<u8>,
    extent: [u32; 2],
    payload_len: usize,
    sh_order: ProbeShOrder,
    scenarios: usize,
}
impl BakedLighting {
    pub const MAGIC: [u8; 8] = *b"RBKLGI01";
    const SINGLE_MAGIC: [u8; 8] = *b"RBKLST01";
    pub const MAX_DEPTH: usize = 10;
    pub const NODE_TEXELS: usize = 3;
    pub const MAX_SCENARIOS: usize = 16;
    const SEQUENCE_MAGIC: [u8; 8] = *b"RBKLTS01";

    pub fn from_bytes(file: &[u8]) -> Result<Self, GraphError> {
        let (magic, payload) = file
            .split_at_checked(8)
            .ok_or_else(|| GraphError::invalid_input("truncated diffuse atlas"))?;
        let matches_magic = payload
            .get(28..32)
            .and_then(|b| b.try_into().ok())
            .and_then(|b| ProbeShOrder::from_atlas_tag(f32::from_le_bytes(b)))
            .is_some_and(|(order, scenarios)| order.file_magic(scenarios) == magic);
        if payload.len() > 256 * 16384 * 16 || !matches_magic {
            return Err(GraphError::invalid_input(
                "invalid diffuse atlas header or size",
            ));
        }
        Self::from_texels(payload.to_vec())
    }

    /// GPU-order RGBA32F little-endian texels, without final row padding.
    pub fn from_texels(mut bytes: Vec<u8>) -> Result<Self, GraphError> {
        let invalid = || GraphError::invalid_input("invalid probe volume layout or irradiance");
        if !bytes.len().is_multiple_of(16) || bytes.len() < 64 || bytes.len() > 256 * 16384 * 16 {
            return Err(invalid());
        }
        let word = |n: usize| f32::from_le_bytes(bytes[n * 4..n * 4 + 4].try_into().unwrap());
        let integer = |n| {
            let v = word(n);
            if (0.0..=16777215.).contains(&v) && v.fract() == 0. {
                Ok(v as usize)
            } else {
                Err(invalid())
            }
        };
        if !bytes
            .as_chunks::<4>()
            .0
            .iter()
            .all(|b| f32::from_le_bytes(*b).is_finite())
            || word(3) <= 0.
        {
            return Err(invalid());
        }
        let nodes = integer(4)?;
        let probes = integer(5)?;
        let (sh_order, scenarios) = ProbeShOrder::from_atlas_tag(word(7)).ok_or_else(invalid)?;
        let header = 2 + scenarios.max(2);
        let probe_texels = 2 + sh_order.coefficient_count() * scenarios;
        let surface_start = header + Self::NODE_TEXELS * nodes + probe_texels * probes;
        let charts = integer(6)?;
        let count = bytes.len() / 16;
        if nodes == 0 || probes == 0 || charts != 0 || surface_start != count {
            return Err(invalid());
        }
        if (2..header).any(|t| (0..3).any(|c| word(t * 4 + c) < 0.)) {
            return Err(invalid());
        }
        // Children are allocated eight at a time in breadth-first order.
        let mut next = 1;
        let mut level_end = 1;
        let mut depth = 0;
        for n in 0..nodes {
            if n == level_end {
                level_end = next;
                depth += 1;
            }
            if n >= next || depth > Self::MAX_DEPTH {
                return Err(invalid());
            }
            let at = (header + n * Self::NODE_TEXELS) * 4;
            let children = integer(at)?;
            if children == 0 {
                for i in 4..12 {
                    if integer(at + i)? >= probes {
                        return Err(invalid());
                    }
                }
            } else {
                if children != next || next + 8 > nodes {
                    return Err(invalid());
                }
                next += 8;
            }
        }
        for i in 0..probes {
            let start = 4 * (header + nodes * Self::NODE_TEXELS + i * probe_texels);
            if !matches!(word(start + 3), 0. | 1.)
                || [0, 1, 2, 4, 5, 6].into_iter().any(|c| word(start + c) < 0.)
            {
                return Err(invalid());
            }
        }
        let width = count.min(256);
        let height = count.div_ceil(width);
        let payload_len = bytes.len();
        bytes.resize(width * height * 16, 0);
        Ok(Self {
            bytes,
            extent: [width as u32, height as u32],
            payload_len,
            sh_order,
            scenarios,
        })
    }
    /// Uniform probe irradiance for each lighting scenario.
    pub fn uniform<const N: usize>(fallback: [[f32; 3]; N]) -> Result<Self, GraphError> {
        if !(1..=Self::MAX_SCENARIOS).contains(&N)
            || fallback.iter().flatten().any(|v| !v.is_finite() || *v < 0.)
        {
            return Err(GraphError::invalid_input("invalid fallback irradiance"));
        }
        let header = 2 + N.max(2);
        let mut texels = vec![[0f32; 4]; header + 5 + 4 * N];
        texels[0][3] = 1.;
        texels[1] = [1., 1., 0., ProbeShOrder::L1.atlas_tag(N)];
        texels[header + 3] = [1.; 4];
        texels[header + 4] = [1.; 4];
        for (i, rgb) in fallback.into_iter().enumerate() {
            texels[2 + i][..3].copy_from_slice(&rgb);
            texels[header + 5 + 4 * i][..3].copy_from_slice(&rgb);
        }
        if N == 1 {
            texels[3] = texels[2];
        }
        Self::from_texels(
            texels
                .iter()
                .copied()
                .flatten()
                .flat_map(f32::to_le_bytes)
                .collect(),
        )
    }
    pub fn image_desc(&self) -> sashiko_renderer_core::graph::ImageDesc {
        sashiko_renderer_core::graph::ImageDesc::rgba32_float(self.extent[0], self.extent[1])
    }
    pub fn upload(
        &self,
        runtime: &mut sashiko_renderer_core::runtime::Runtime,
        pool: &mut sashiko_renderer_core::graph::GraphPool,
    ) -> Result<
        (
            sashiko_renderer_core::runtime::FrameId,
            sashiko_renderer_core::runtime::ManagedImage,
        ),
        sashiko_renderer_core::runtime::TextureUploadError,
    > {
        runtime.upload_texture(
            pool,
            "baked indirect light",
            self.image_desc(),
            self.rgba32f(),
        )
    }
    pub fn scenario_count(&self) -> usize {
        self.scenarios
    }
    pub fn sh_order(&self) -> ProbeShOrder {
        self.sh_order
    }
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
    pub fn into_rgba32f(self) -> Vec<u8> {
        self.bytes
    }
    pub fn rgba32f(&self) -> &[u8] {
        &self.bytes
    }
    pub fn write(&self, mut output: impl std::io::Write) -> std::io::Result<()> {
        output.write_all(&self.sh_order.file_magic(self.scenarios))?;
        output.write_all(&self.bytes[..self.payload_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_files_preserve_order_scenarios_and_all_coefficients() {
        for keys in 1..=BakedLighting::MAX_SCENARIOS {
            let l1_magic = match keys {
                1 => b"RBKLST01",
                2 => b"RBKLGI01",
                _ => b"RBKLTS01",
            };
            for (order, magic, tag, coeffs) in [
                (
                    ProbeShOrder::L1,
                    l1_magic,
                    if keys == 2 { 0. } else { keys as f32 },
                    4,
                ),
                (ProbeShOrder::L2, b"RBKLSH02", -(keys as f32), 9),
            ] {
                let header = 2 + keys.max(2);
                let mut texels = vec![[0f32; 4]; header + 3 + 2 + keys * coeffs];
                texels[0][3] = 1.;
                texels[1] = [1., 1., 0., tag];
                texels[header + 3][3] = 1.;
                // The highest coefficient of the last scenario can be negative.
                texels.last_mut().unwrap()[2] = -1.5;
                let mut file = magic.to_vec();
                file.extend(texels.into_iter().flatten().flat_map(f32::to_le_bytes));
                let atlas = BakedLighting::from_bytes(&file).unwrap();
                assert_eq!(atlas.sh_order(), order);
                assert_eq!(atlas.scenario_count(), keys);
                let mut written = Vec::new();
                atlas.write(&mut written).unwrap();
                assert_eq!(written, file);

                if order == ProbeShOrder::L2 {
                    file[..8].copy_from_slice(l1_magic);
                    assert!(BakedLighting::from_bytes(&file).is_err());
                    file[..8].copy_from_slice(magic);
                    file.truncate(file.len() - 16);
                    assert!(BakedLighting::from_bytes(&file).is_err());
                }
            }
        }
    }

    #[test]
    fn sequence_roundtrips_and_rejects_mismatched_or_truncated_keys() {
        let atlas = BakedLighting::uniform([[0.; 3]; BakedLighting::MAX_SCENARIOS]).unwrap();
        let mut file = Vec::new();
        atlas.write(&mut file).unwrap();
        assert_eq!(
            BakedLighting::from_bytes(&file).unwrap().scenario_count(),
            BakedLighting::MAX_SCENARIOS
        );
        let mut bad = file.clone();
        bad[..8].copy_from_slice(&BakedLighting::MAGIC);
        assert!(BakedLighting::from_bytes(&bad).is_err());
        for tag in [0., 1., 2., 3.5, 17., f32::NAN] {
            bad.copy_from_slice(&file);
            bad[36..40].copy_from_slice(&tag.to_le_bytes());
            assert!(BakedLighting::from_bytes(&bad).is_err());
        }
        assert!(BakedLighting::from_bytes(&file[..file.len() - 16]).is_err());
    }
}

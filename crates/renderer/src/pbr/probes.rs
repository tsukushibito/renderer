//! Cooked first-order spherical harmonic irradiance. Lighting scenarios share one
//! world-space octree, eight corner probes per leaf and visibility data.
use renderer_core::graph::GraphError;

/// Interpretation of the environment's diffuse image.
#[derive(Clone, Copy, Debug, Default)]
pub enum DiffuseLighting {
    #[default]
    EnvironmentMap,
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
            Self::BakedLighting { blend, view } if (0.0..=1.0).contains(&blend) => {
                Ok(1. + 2. * (view as u32 as f32) + blend)
            }
            Self::BakedLighting { .. } => {
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
        let tag = payload
            .get(28..32)
            .and_then(|b| b.try_into().ok())
            .map(f32::from_le_bytes);
        let matches = match tag {
            Some(0.) => magic == Self::MAGIC,
            Some(1.) => magic == Self::SINGLE_MAGIC,
            Some(n) if n >= 3. && n <= Self::MAX_SCENARIOS as f32 && n.fract() == 0. => {
                magic == Self::SEQUENCE_MAGIC
            }
            _ => false,
        };
        if payload.len() > 256 * 16384 * 16 || !matches {
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
        let scenarios = match word(7) {
            0. => 2,
            1. => 1,
            n if (3.0..=Self::MAX_SCENARIOS as f32).contains(&n) && n.fract() == 0. => n as usize,
            _ => return Err(invalid()),
        };
        let header = 2 + scenarios.max(2);
        let probe_texels = 2 + 4 * scenarios;
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
        texels[1] = [1., 1., 0., if N == 2 { 0. } else { N as f32 }];
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
    pub fn image_desc(&self) -> renderer_core::graph::ImageDesc {
        renderer_core::graph::ImageDesc::rgba32_float(self.extent[0], self.extent[1])
    }
    pub fn upload(
        &self,
        runtime: &mut renderer_core::runtime::Runtime,
        pool: &mut renderer_core::graph::GraphPool,
    ) -> Result<
        (
            renderer_core::runtime::FrameId,
            renderer_core::runtime::ManagedImage,
        ),
        renderer_core::runtime::TextureUploadError,
    > {
        runtime.upload_texture(
            pool,
            "baked indirect light",
            self.image_desc(),
            self.rgba32f(),
        )
    }
    pub fn scenario_count(&self) -> usize {
        match f32::from_le_bytes(self.bytes[28..32].try_into().unwrap()) as usize {
            0 => 2,
            count => count,
        }
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
        output.write_all(match self.scenario_count() {
            1 => &Self::SINGLE_MAGIC,
            2 => &Self::MAGIC,
            _ => &Self::SEQUENCE_MAGIC,
        })?;
        output.write_all(&self.bytes[..self.payload_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

//! Cooked first-order spherical harmonic irradiance. One or two scenarios use one
//! world-space octree, eight corner probes per leaf and visibility data.
use renderer_core::graph::GraphError;

/// Interpretation of the environment's diffuse image.
#[derive(Clone, Copy, Debug, Default)]
pub enum DiffuseLighting {
    #[default]
    EnvironmentMap,
    /// Upload the diffuse texture from a validated BakedLighting. Zero selects
    /// the first baked scenario and one selects the second. A single scenario
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

/// A private UV chart with an independent bilinear footprint. Corner UVs are
/// (0,0), (1,0), (0,1) in triangle order; material UVs are independent.
pub struct LightmapChart<const N: usize = 2> {
    pub extent: [u16; 2],
    pub irradiance: Vec<[[f32; 3]; N]>,
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
    pub const HEADER_TEXELS: usize = 4;

    pub fn from_bytes(file: &[u8]) -> Result<Self, GraphError> {
        let (magic, payload) = file
            .split_at_checked(8)
            .ok_or_else(|| GraphError::invalid_input("truncated diffuse atlas"))?;
        let expected = if magic == Self::MAGIC {
            0f32
        } else if magic == Self::SINGLE_MAGIC {
            1.
        } else {
            return Err(GraphError::invalid_input("unknown diffuse atlas version"));
        };
        if payload.len() > 256 * 16384 * 16 || payload.get(28..32) != Some(&expected.to_le_bytes())
        {
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
            || [8, 9, 10, 12, 13, 14].into_iter().any(|i| word(i) < 0.)
        {
            return Err(invalid());
        }
        let nodes = integer(4)?;
        let probes = integer(5)?;
        let scenarios = match word(7) {
            0. => 2,
            1. => 1,
            _ => return Err(invalid()),
        };
        let probe_texels = 2 + 4 * scenarios;
        let surface_start = Self::HEADER_TEXELS + Self::NODE_TEXELS * nodes + probe_texels * probes;
        let charts = integer(6)?;
        let count = bytes.len() / 16;
        if nodes == 0 || probes == 0 || surface_start + charts > count {
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
            let at = (Self::HEADER_TEXELS + n * Self::NODE_TEXELS) * 4;
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
            let start = 4 * (Self::HEADER_TEXELS + nodes * Self::NODE_TEXELS + i * probe_texels);
            if !matches!(word(start + 3), 0. | 1.)
                || [0, 1, 2, 4, 5, 6].into_iter().any(|c| word(start + c) < 0.)
            {
                return Err(invalid());
            }
        }
        let mut end = surface_start + charts;
        for i in 0..charts {
            let at = (surface_start + i) * 4;
            let w = integer(at + 1)?;
            let h = integer(at + 2)?;
            if integer(at)? != end || !(2..=256).contains(&w) || !(2..=256).contains(&h) {
                return Err(invalid());
            }
            end += scenarios * w * h;
            if end > count {
                return Err(invalid());
            }
        }
        if end != count
            || (surface_start + charts..end).any(|t| (0..3).any(|c| word(4 * t + c) < 0.))
        {
            return Err(invalid());
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
    /// Attach RGB irradiance for static charts, in baked triangle order. Each
    /// row-major texel has one RGB value per lighting scenario.
    pub fn with_lightmaps<const N: usize>(
        mut self,
        charts: &[LightmapChart<N>],
    ) -> Result<Self, GraphError> {
        let invalid = || GraphError::invalid_input("invalid static lightmap atlas");
        if self.bytes[24..28] != 0f32.to_le_bytes() || self.scenario_count() != N {
            return Err(invalid());
        }
        self.bytes.truncate(self.payload_len);
        let mut end = self.payload_len / 16 + charts.len();
        if end > 256 * 16384 {
            return Err(invalid());
        }
        self.bytes[24..28].copy_from_slice(&(charts.len() as f32).to_le_bytes());
        for c in charts {
            let [w, h] = c.extent;
            if !(2..=256).contains(&w)
                || !(2..=256).contains(&h)
                || c.irradiance.len() != usize::from(w) * usize::from(h)
            {
                return Err(invalid());
            }
            self.bytes.extend(
                [end as f32, w as f32, h as f32, 0.]
                    .into_iter()
                    .flat_map(f32::to_le_bytes),
            );
            end += N * c.irradiance.len();
            if end > 256 * 16384 {
                return Err(invalid());
            }
        }
        for c in charts {
            for scenarios in &c.irradiance {
                for rgb in scenarios {
                    if rgb.iter().any(|x| !x.is_finite() || *x < 0.) {
                        return Err(invalid());
                    }
                    self.bytes.extend(
                        [rgb[0], rgb[1], rgb[2], 0.]
                            .into_iter()
                            .flat_map(f32::to_le_bytes),
                    );
                }
            }
        }
        Self::from_texels(self.bytes)
    }
    /// Build a surface-only atlas. Unmapped surfaces use the specified uniform
    /// fallback irradiance for each scenario.
    pub fn from_lightmaps<const N: usize>(
        charts: &[LightmapChart<N>],
        fallback: [[f32; 3]; N],
    ) -> Result<Self, GraphError> {
        if !(1..=2).contains(&N) || fallback.iter().flatten().any(|v| !v.is_finite() || *v < 0.) {
            return Err(GraphError::invalid_input("invalid fallback irradiance"));
        }
        let mut texels = [[0f32; 4]; 17];
        texels[0][3] = 1.;
        texels[1] = [1., 1., 0., f32::from(N == 1)];
        texels[7] = [1.; 4];
        texels[8] = [1.; 4];
        for (i, rgb) in fallback.into_iter().enumerate() {
            texels[2 + i][..3].copy_from_slice(&rgb);
            texels[9 + 4 * i][..3].copy_from_slice(&rgb);
        }
        if N == 1 {
            texels[3] = texels[2];
        }
        Self::from_texels(
            texels[..9 + 4 * N]
                .iter()
                .copied()
                .flatten()
                .flat_map(f32::to_le_bytes)
                .collect(),
        )?
        .with_lightmaps(charts)
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
        if self.bytes[28..32] == 1f32.to_le_bytes() {
            1
        } else {
            2
        }
    }
    pub fn chart_count(&self) -> u32 {
        f32::from_le_bytes(self.bytes[24..28].try_into().unwrap()) as u32
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
        output.write_all(if self.scenario_count() == 1 {
            &Self::SINGLE_MAGIC
        } else {
            &Self::MAGIC
        })?;
        output.write_all(&self.bytes[..self.payload_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_only_archive_round_trips_and_rejects_negative_irradiance() {
        let chart = LightmapChart {
            extent: [2, 2],
            irradiance: vec![[[2., 1., 0.], [0., 0., 0.5]]; 4],
        };
        let atlas = BakedLighting::from_lightmaps(&[chart], [[0.5; 3], [0.1; 3]]).unwrap();
        let mut file = Vec::new();
        atlas.write(&mut file).unwrap();
        assert_eq!(BakedLighting::from_bytes(&file).unwrap().chart_count(), 1);
        let last_blue = file.len() - 8;
        file[last_blue..last_blue + 4].copy_from_slice(&(-1f32).to_le_bytes());
        assert!(BakedLighting::from_bytes(&file).is_err());
    }
}

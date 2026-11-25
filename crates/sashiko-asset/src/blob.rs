use crate::invalid;
use sashiko_renderer::{core::program::SamplerDescriptor, pbr::MaterialFactors};
use serde::{Deserialize, Serialize};
use std::io;

pub(super) const MESH: &[u8] = b"RMESH001";
pub(super) const IMAGE: &[u8] = b"RIMAG001";
const MATERIAL: &[u8] = b"RMATL001";

fn word(bytes: &mut &[u8]) -> io::Result<u32> {
    let value = bytes
        .split_off(..4)
        .ok_or_else(|| invalid("truncated asset header"))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

/// Model-local positions, normals, tangents (including sign), and first UV channel.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshBlob {
    pub vertices: Vec<[f32; 12]>,
    pub indices: Vec<u32>,
}

impl MeshBlob {
    fn check(&self) -> io::Result<()> {
        if self.vertices.is_empty()
            || self.indices.is_empty()
            || !self.indices.len().is_multiple_of(3)
            || self
                .indices
                .iter()
                .any(|&i| i as usize >= self.vertices.len())
        {
            return Err(invalid("invalid triangle mesh indices"));
        }
        if !self.vertices.iter().flatten().all(|v| v.is_finite()) {
            return Err(invalid("nonfinite mesh attribute"));
        }
        Ok(())
    }
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.check()?;
        let mut out = MESH.to_vec();
        out.extend(
            u32::try_from(self.vertices.len())
                .map_err(invalid)?
                .to_le_bytes(),
        );
        out.extend(
            u32::try_from(self.indices.len())
                .map_err(invalid)?
                .to_le_bytes(),
        );
        out.extend(self.vertices.iter().flatten().flat_map(|v| v.to_le_bytes()));
        out.extend(self.indices.iter().flat_map(|v| v.to_le_bytes()));
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut payload = bytes
            .strip_prefix(MESH)
            .ok_or_else(|| invalid("unknown mesh format"))?;
        let nv = u64::from(word(&mut payload)?);
        let ni = u64::from(word(&mut payload)?);
        if payload.len() as u64 != nv * 48 + ni * 4 {
            return Err(invalid("mesh size mismatch"));
        }
        let (vertices, indices) = payload.split_at((nv * 48) as usize);
        let mesh = Self {
            vertices: vertices
                .as_chunks::<48>()
                .0
                .iter()
                .map(|v| {
                    std::array::from_fn(|i| {
                        f32::from_le_bytes(v[i * 4..i * 4 + 4].try_into().unwrap())
                    })
                })
                .collect(),
            indices: indices
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&v| u32::from_le_bytes(v))
                .collect(),
        };
        mesh.check()?;
        Ok(mesh)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sampler {
    pub wrap_s: u32,
    pub wrap_t: u32,
    pub mag_filter: Option<u32>,
    pub min_filter: Option<u32>,
}

impl Sampler {
    pub fn descriptor(&self) -> io::Result<SamplerDescriptor> {
        SamplerDescriptor::from_gltf(
            Some(self.wrap_s),
            Some(self.wrap_t),
            self.mag_filter,
            self.min_filter,
        )
        .map_err(invalid)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialBlob {
    pub uniform: Vec<u8>,
    pub samplers: [Option<Sampler>; 4],
}

impl MaterialBlob {
    pub fn factors(&self) -> io::Result<MaterialFactors> {
        let uniform = self
            .uniform
            .as_slice()
            .try_into()
            .map_err(|_| invalid("invalid PBR uniform length"))?;
        MaterialFactors::decode(uniform).map_err(invalid)
    }
    fn check(&self) -> io::Result<()> {
        self.factors()?;
        for sampler in self.samplers.iter().flatten() {
            sampler.descriptor()?;
        }
        Ok(())
    }
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.check()?;
        let mut out = MATERIAL.to_vec();
        serde_json::to_writer(&mut out, self).map_err(invalid)?;
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let data = bytes
            .strip_prefix(MATERIAL)
            .ok_or_else(|| invalid("unknown material format"))?;
        let mat: Self = serde_json::from_slice(data).map_err(invalid)?;
        mat.check()?;
        Ok(mat)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageBlob {
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub srgb: bool,
    /// Tightly packed RGBA8 levels, largest first.
    pub pixels: Vec<u8>,
}

fn check_image(width: u32, height: u32, levels: u32, size: usize) -> io::Result<()> {
    if width == 0 || height == 0 || levels == 0 || levels > width.max(height).ilog2() + 1 {
        return Err(invalid("invalid image extent or mip count"));
    }
    let mut expected = 0_u64;
    for mip in 0..levels {
        let pixels = u64::from((width >> mip).max(1)) * u64::from((height >> mip).max(1));
        expected = pixels
            .checked_mul(4)
            .and_then(|n| expected.checked_add(n))
            .ok_or_else(|| invalid("image size overflow"))?;
    }
    if expected != size as u64 {
        return Err(invalid("incorrect image payload size"));
    }
    Ok(())
}

impl ImageBlob {
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        check_image(self.width, self.height, self.mip_levels, self.pixels.len())?;
        let mut out = IMAGE.to_vec();
        for v in [
            self.width,
            self.height,
            self.mip_levels,
            u32::from(self.srgb),
        ] {
            out.extend(v.to_le_bytes());
        }
        out.extend_from_slice(&self.pixels);
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut data = bytes
            .strip_prefix(IMAGE)
            .ok_or_else(|| invalid("unknown image format"))?;
        let width = word(&mut data)?;
        let height = word(&mut data)?;
        let mip_levels = word(&mut data)?;
        let srgb = match word(&mut data)? {
            0 => false,
            1 => true,
            _ => return Err(invalid("unsupported image format")),
        };
        check_image(width, height, mip_levels, data.len())?;
        Ok(Self {
            width,
            height,
            mip_levels,
            srgb,
            pixels: data.to_vec(),
        })
    }
}

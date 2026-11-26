//! Standard renderer conversions for cooked equipment.
use crate::{ImageBlob, MaterialSlot, MeshBlob, invalid};
use sashiko_renderer::{
    core::{graph::ImageDesc, program::SamplerDescriptor},
    material::{MaterialTextures, PbrParams},
    mesh::MeshData,
    pbr::{MaterialFactors, PbrVertex},
    pipeline::PipelineResult,
};
use serde::{Deserialize, Serialize};
use std::io;

const MATERIAL: &[u8] = b"RMATL001";

impl MeshBlob {
    pub fn mesh_data(&self) -> PipelineResult<MeshData> {
        let vertices = self
            .vertices
            .iter()
            .map(|v| PbrVertex::new(v.position, v.normal, v.tangent, v.uv))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MeshData::pbr(&vertices, &self.indices)?)
    }
}

impl ImageBlob {
    /// Check the stored transfer function against the material channel at load time.
    pub fn texture_desc(&self, slot: MaterialSlot) -> io::Result<ImageDesc> {
        if self.srgb != slot.is_srgb() {
            return Err(invalid("image color space does not match material slot"));
        }
        let mut desc = if self.srgb {
            ImageDesc::rgba8_srgb(self.width, self.height)
        } else {
            ImageDesc::rgba8(self.width, self.height)
        };
        desc.mip_levels = self.mip_levels;
        Ok(desc)
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

impl MaterialBlob {
    pub fn parameters(&self) -> io::Result<PbrParams> {
        let sampler = |slot: MaterialSlot| {
            self.samplers[slot.index()]
                .map(|s| s.descriptor())
                .transpose()
                .map(|s| s.unwrap_or(SamplerDescriptor::LinearRepeat))
        };
        Ok(PbrParams {
            factors: self.factors()?,
            samplers: MaterialTextures {
                base_color: sampler(MaterialSlot::BaseColor)?,
                normal: sampler(MaterialSlot::Normal)?,
                orm: sampler(MaterialSlot::Orm)?,
                emissive: sampler(MaterialSlot::Emissive)?,
            },
            ..Default::default()
        })
    }
}

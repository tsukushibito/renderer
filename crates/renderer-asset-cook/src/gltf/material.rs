use super::{ImageRole, Result};
use renderer::{material::AlphaMode, pbr::MaterialFactors};
use renderer_asset::{MaterialBlob, Sampler};

pub(super) struct SourceMaterial {
    factors: MaterialFactors,
    samplers: [Option<Sampler>; 4],
    pub(super) images: [Option<(usize, ImageRole)>; 4],
}

impl SourceMaterial {
    pub(super) fn read(source: gltf::Material<'_>) -> Result<Self> {
        let pbr = source.pbr_metallic_roughness();
        let normal = source.normal_texture();
        let occlusion = source.occlusion_texture();
        let orm = pbr.metallic_roughness_texture();
        if let Some(ao) = &occlusion
            && orm.as_ref().is_none_or(|mr| {
                mr.texture().index() != ao.texture().index() || mr.tex_coord() != ao.tex_coord()
            })
        {
            return Err(format!("material {:?}: occlusion and metallicRoughness must use a shared packed ORM texture and UV set", source.index()).into());
        }
        // When glTF has metallicRoughness but no occlusion input, R is unspecified.
        let factors = MaterialFactors {
            base_color: pbr.base_color_factor(),
            emissive_nits: source.emissive_factor(),
            normal_scale: normal.as_ref().map_or(1., |n| n.scale()),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            occlusion_strength: occlusion
                .as_ref()
                .map_or(if orm.is_some() { 0. } else { 1. }, |o| o.strength()),
            alpha_cutoff: source.alpha_cutoff().unwrap_or(0.5),
            alpha_mode: match source.alpha_mode() {
                gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
                gltf::material::AlphaMode::Mask => AlphaMode::Mask,
                gltf::material::AlphaMode::Blend => AlphaMode::Blend,
            },
            double_sided: source.double_sided(),
            ..Default::default()
        };
        if factors
            .emissive_nits
            .iter()
            .any(|v| !(0.0..=1.0).contains(v))
        {
            return Err(format!("material {:?}: invalid emissive factor", source.index()).into());
        }
        let slots = [
            pbr.base_color_texture()
                .map(|t| (t.texture(), t.tex_coord(), ImageRole::Color)),
            normal.map(|t| (t.texture(), t.tex_coord(), ImageRole::Normal)),
            orm.map(|t| (t.texture(), t.tex_coord(), ImageRole::Orm)),
            source
                .emissive_texture()
                .map(|t| (t.texture(), t.tex_coord(), ImageRole::Color)),
        ];
        let mut images = [None; 4];
        let mut samplers = std::array::from_fn(|_| None);
        for (slot, info) in slots.into_iter().enumerate() {
            if let Some((texture, uv, role)) = info {
                if uv != 0 {
                    return Err(format!(
                        "material {:?} uses unsupported TEXCOORD_{uv}",
                        source.index()
                    )
                    .into());
                }
                let sampler = texture.sampler();
                samplers[slot] = Some(Sampler {
                    wrap_s: sampler.wrap_s().as_gl_enum(),
                    wrap_t: sampler.wrap_t().as_gl_enum(),
                    min_filter: sampler.min_filter().map(|f| f.as_gl_enum()),
                    mag_filter: sampler.mag_filter().map(|f| f.as_gl_enum()),
                });
                images[slot] = Some((texture.source().index(), role));
            }
        }
        Ok(Self {
            factors,
            images,
            samplers,
        })
    }

    pub(super) fn inputs(&self) -> Result<Vec<u8>> {
        // Normalized semantic inputs exclude image identity and pixel contents.
        // Rebinding an image only changes the catalog unless the sampler also changes.
        let f = self.factors;
        Ok(serde_json::to_vec(&(
            f.base_color,
            f.emissive_nits,
            f.normal_scale,
            f.metallic,
            f.roughness,
            f.occlusion_strength,
            f.alpha_cutoff,
            format!("{:?}", f.alpha_mode),
            f.double_sided,
            self.samplers.each_ref().map(|s| {
                s.as_ref()
                    .map(|s| (s.wrap_s, s.wrap_t, s.mag_filter, s.min_filter))
            }),
        ))?)
    }

    pub(super) fn cook(&self) -> Result<Vec<u8>> {
        Ok(MaterialBlob {
            uniform: self.factors.encode()?.to_vec(),
            samplers: self.samplers.clone(),
        }
        .encode()?)
    }
}

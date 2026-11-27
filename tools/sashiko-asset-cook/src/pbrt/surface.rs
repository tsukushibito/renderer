use super::{Command, Result, Textures};
use crate::{
    image::{self, ImageRole},
    scene::object,
};
use sashiko_asset::scene::{Scattering, Surface};
use sashiko_asset::{BlobRef, MaterialBlob, Sampler};
use sashiko_renderer::{material::AlphaMode, pbr::MaterialFactors};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(super) struct Material {
    kind: String,
    rgb: [f32; 3],
    color: Option<PathBuf>,
    transmission: Option<PathBuf>,
    displacement: Option<PathBuf>,
    eta: f32,
    roughness: f32,
    scale: f32,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            kind: "diffuse".into(),
            rgb: [0.5; 3],
            color: None,
            transmission: None,
            displacement: None,
            eta: 1.5,
            roughness: 0.,
            scale: 1.,
        }
    }
}

impl Material {
    pub(super) fn parse(cmd: &Command<'_>, kind: &str, textures: &Textures) -> Result<Self> {
        if !matches!(
            kind,
            "coateddiffuse" | "diffusetransmission" | "dielectric" | "diffuse" | "conductor"
        ) {
            return Err(format!("unsupported PBRT material {kind}").into());
        }
        for key in cmd.params.keys() {
            if !matches!(
                key.as_ref(),
                "string type"
                    | "texture reflectance"
                    | "texture transmittance"
                    | "texture displacement"
                    | "float eta"
                    | "float scale"
                    | "float roughness"
                    | "rgb reflectance"
            ) {
                return Err(format!("unsupported material parameter {key}").into());
            }
        }
        let texture = |key| -> Result<Option<PathBuf>> {
            cmd.param(key)?
                .map(|name| {
                    (if key == "texture displacement" {
                        &textures.scalar
                    } else {
                        &textures.spectrum
                    })
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("unknown {key} texture {name}").into())
                })
                .transpose()
        };
        let color = texture("texture reflectance")?;
        if kind == "conductor"
            && ((color.is_none() && !cmd.params.contains_key("rgb reflectance"))
                || cmd.params.contains_key("float eta"))
        {
            return Err("conductor requires an explicit reflectance without eta/k".into());
        }
        let rgb = if let Some(rgb) = cmd.params.get("rgb reflectance") {
            let [r, g, b] = rgb.as_slice() else {
                return Err("expected RGB reflectance triplet".into());
            };
            [r.parse()?, g.parse()?, b.parse()?]
        } else if color.is_some() {
            [1.; 3]
        } else if kind == "diffusetransmission" {
            [0.25; 3]
        } else {
            [0.5; 3]
        };
        Ok(Self {
            kind: kind.into(),
            rgb,
            color,
            transmission: texture("texture transmittance")?,
            displacement: texture("texture displacement")?,
            eta: cmd.param("float eta")?.unwrap_or("1.5").parse()?,
            roughness: cmd.param("float roughness")?.unwrap_or("0").parse()?,
            scale: cmd.param("float scale")?.unwrap_or("1").parse()?,
        })
    }
}

#[derive(Default)]
pub(super) struct Materials {
    images: HashMap<(Option<PathBuf>, Option<PathBuf>), BlobRef>,
    originals: HashMap<(PathBuf, ImageRole), BlobRef>,
}

impl Materials {
    fn source_map(
        &mut self,
        out: &Path,
        path: Option<&Path>,
        role: ImageRole,
    ) -> Result<Option<BlobRef>> {
        let Some(path) = path else { return Ok(None) };
        let key = (path.to_owned(), role);
        if let Some(image) = self.originals.get(&key) {
            return Ok(Some(*image));
        }
        let source =
            image::decode(&fs::read(path)?).map_err(|e| format!("{}: {e}", path.display()))?;
        let pixels = if role == ImageRole::Color {
            source.rgba
        } else {
            source
                .pbrt_scalar()
                .into_iter()
                .flat_map(|v| [v, v, v, 255])
                .collect()
        };
        let encoded = image::cook_rgba(source.width, source.height, pixels, role)?;
        let image = object(out, &encoded)?;
        self.originals.insert(key, image);
        Ok(Some(image))
    }
    fn color(
        &mut self,
        out: &Path,
        color: Option<&Path>,
        alpha: Option<&Path>,
    ) -> Result<Option<BlobRef>> {
        if color.is_none() && alpha.is_none() {
            return Ok(None);
        }
        let key = (color.map(Path::to_owned), alpha.map(Path::to_owned));
        if let Some(reference) = self.images.get(&key) {
            return Ok(Some(*reference));
        }
        let load = |p: &Path| -> Result<_> {
            image::decode(&fs::read(p)?).map_err(|e| format!("{}: {e}", p.display()).into())
        };
        let mask = alpha
            .map(load)
            .transpose()?
            .map(|s| (s.width, s.height, s.pbrt_scalar()));
        let (width, height, mut rgba) = if let Some(path) = color {
            let source = load(path)?;
            (source.width, source.height, source.rgba)
        } else {
            let (w, h, scalar) = mask.as_ref().unwrap();
            (*w, *h, vec![255; scalar.len() * 4])
        };
        for (i, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            pixel[3] = match &mask {
                Some((w, h, mask)) => {
                    let x = i % width as usize;
                    let y = i / width as usize;
                    let sx = (2 * x as u64 + 1) * u64::from(*w) / (2 * u64::from(width));
                    let sy = (2 * y as u64 + 1) * u64::from(*h) / (2 * u64::from(height));
                    mask[(sy * u64::from(*w) + sx) as usize]
                }
                None => 255,
            };
        }
        let encoded = if mask.is_some() {
            image::cook_cutout(width, height, rgba)?
        } else {
            image::cook_rgba(width, height, rgba, ImageRole::Color)?
        };
        let reference = object(out, &encoded)?;
        self.images.insert(key, reference);
        Ok(Some(reference))
    }

    pub(super) fn cook(
        &mut self,
        out: &Path,
        mat: &Material,
        alpha: Option<&Path>,
        location: &str,
        notes: &mut Vec<String>,
    ) -> Result<(BlobRef, Option<BlobRef>, Surface)> {
        let color = self.color(out, mat.color.as_deref(), alpha)?;
        let factors = MaterialFactors {
            base_color: [mat.rgb[0], mat.rgb[1], mat.rgb[2], 1.],
            metallic: if mat.kind == "conductor" { 1. } else { 0. },
            roughness: if matches!(mat.kind.as_str(), "coateddiffuse" | "conductor") {
                mat.roughness
            } else {
                1.
            },
            double_sided: true,
            alpha_mode: if alpha.is_some() {
                AlphaMode::Mask
            } else {
                AlphaMode::Opaque
            },
            alpha_cutoff: 0.5,
            ..Default::default()
        };
        if mat.kind != "diffuse" || alpha.is_some() || mat.displacement.is_some() {
            notes.push(format!("{location}: interactive approximation {} -> PBR (eta {}, roughness {}, scale {}, transmission {:?}, displacement {:?}, alpha {:?} -> 0.5 cutoff)",
                mat.kind,mat.eta,mat.roughness,mat.scale,mat.transmission,mat.displacement,alpha));
        }
        let sampler = Sampler {
            wrap_s: 10497,
            wrap_t: 10497,
            mag_filter: Some(9729),
            min_filter: Some(9987),
        };
        let blob = MaterialBlob {
            factors,
            samplers: [color.map(|_| sampler), None, None, None],
        };
        let scattering = match mat.kind.as_str() {
            "diffuse" => Scattering::Diffuse,
            "conductor" => Scattering::Conductor,
            "coateddiffuse" => Scattering::CoatedDiffuse,
            "diffusetransmission" => Scattering::DiffuseTransmission,
            "dielectric" => Scattering::Dielectric,
            other => return Err(format!("{location}: unsupported scattering {other}").into()),
        };
        let surface = Surface {
            scattering,
            reflectance: mat.rgb,
            color: self.source_map(out, mat.color.as_deref(), ImageRole::Color)?,
            transmittance: self.source_map(out, mat.transmission.as_deref(), ImageRole::Color)?,
            displacement: self.source_map(out, mat.displacement.as_deref(), ImageRole::Scalar)?,
            alpha: self.source_map(out, alpha, ImageRole::Scalar)?,
            eta: mat.eta,
            roughness: mat.roughness,
            scale: mat.scale,
        };
        Ok((object(out, &blob.encode()?)?, color, surface))
    }
}

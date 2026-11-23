//! Isolate terms at publication so a capture still exercises the ordinary forward shader.
use super::{Error, asset, ibl};
use renderer::pbr::{BakedLighting, MaterialFactors};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LightingPass {
    Beauty,
    BaseColor,
    Direct,
    Diffuse,
    DiffuseNoAo,
    Specular,
    LocalSpecular,
    Emissive,
}

impl std::str::FromStr for LightingPass {
    type Err = Error;
    fn from_str(name: &str) -> Result<Self, Error> {
        Ok(match name {
            "beauty" => Self::Beauty,
            "base-color" => Self::BaseColor,
            "direct" => Self::Direct,
            "diffuse" => Self::Diffuse,
            "diffuse-no-ao" => Self::DiffuseNoAo,
            "specular" => Self::Specular,
            "local-specular" => Self::LocalSpecular,
            "emissive" => Self::Emissive,
            _ => return Err(format!("unknown lighting pass {name}").into()),
        })
    }
}

impl LightingPass {
    pub(super) fn has_sun(self) -> bool {
        matches!(self, Self::Beauty | Self::Direct)
    }

    pub(super) fn apply(
        self,
        scene: &mut asset::Scene,
        env: &mut ibl::Environment,
        baked: bool,
    ) -> Result<(), Error> {
        if self == Self::Beauty {
            return Ok(());
        }
        if !matches!(self, Self::Diffuse | Self::DiffuseNoAo) {
            if baked {
                env.lightmaps.clear();
                for primitive in &mut scene.primitives {
                    primitive.surface = None;
                }
                // Keep a valid, black probe volume for the diffuse binding.
                let atlas = BakedLighting::uniform([[0.; 3]])?;
                let [width, height] = atlas.extent();
                let bytes = atlas.into_rgba32f();
                env.diffuse = ibl::Image {
                    width,
                    height,
                    mips: std::iter::once(0..bytes.len()).collect(),
                    format: renderer_core::ImageFormat::Rgba32Float,
                    bytes,
                };
            } else {
                env.diffuse.bytes.fill(0);
            }
        }
        if !matches!(self, Self::Specular | Self::LocalSpecular) {
            env.brdf.bytes.fill(0);
        }
        if self == Self::LocalSpecular {
            env.strength = 0.;
        }
        if self == Self::DiffuseNoAo {
            scene.disable_environment_occlusion();
        }
        for m in &mut scene.materials {
            let mut factors = MaterialFactors::decode(&m.uniform)?;
            if self == Self::BaseColor {
                // Use the emissive input as an unlit read of the original sRGB base texture.
                m.textures[3] = m.textures[0];
                factors.emissive = [
                    factors.base_color[0],
                    factors.base_color[1],
                    factors.base_color[2],
                ];
            } else if self != Self::Emissive {
                factors.emissive = [0.; 3];
            }
            m.uniform = factors.encode()?;
        }
        Ok(())
    }
}

use renderer::{
    Renderer,
    core::{graph::ImageDesc, runtime::ManagedImage},
    material::{MaterialTexture, MaterialTextures, PbrParams},
    pipeline::{AssetPreparation, PipelineResult},
};
use sashiko::animation::AnimatedModel;
use std::time::{Duration, Instant};

const COLOR: ImageDesc = ImageDesc::rgba8_srgb(256, 256).with_mip_levels(9);
const LINEAR: ImageDesc = ImageDesc::rgba8(256, 256).with_mip_levels(9);
fn character_image(model: &AnimatedModel) -> ImageDesc {
    let (_, [width, height]) = model.color();
    ImageDesc::rgba8_srgb(width, height)
}

const LABEL: &str = "lab surface";

struct SurfaceMaps {
    base: &'static [u8],
    normal: &'static [u8],
    orm: &'static [u8],
}

const FLOOR: SurfaceMaps = SurfaceMaps {
    base: include_bytes!("../../../../assets/survivor/environment/floor-base.rgba"),
    normal: include_bytes!("../../../../assets/survivor/environment/floor-normal.rgba"),
    orm: include_bytes!("../../../../assets/survivor/environment/floor-orm.rgba"),
};
const STEEL: SurfaceMaps = SurfaceMaps {
    base: include_bytes!("../../../../assets/survivor/environment/steel-base.rgba"),
    normal: include_bytes!("../../../../assets/survivor/environment/steel-normal.rgba"),
    orm: include_bytes!("../../../../assets/survivor/environment/steel-orm.rgba"),
};

pub(super) fn content_digests() -> [sashiko::asset::Digest; 6] {
    [
        FLOOR.base,
        FLOOR.normal,
        FLOOR.orm,
        STEEL.base,
        STEEL.normal,
        STEEL.orm,
    ]
    .map(sashiko::asset::Digest::of)
}

/// The material registry borrows snapshots. These owners retain the registrations.
pub(super) struct Surfaces {
    pub(super) floor: PbrParams,
    pub(super) steel: PbrParams,
    pub(super) characters: Vec<PbrParams>,
    _images: Vec<ManagedImage>,
}

impl Surfaces {
    #[cfg(all(
        windows,
        feature = "presentation",
        any(feature = "vulkan", feature = "d3d12")
    ))]
    pub(super) fn for_window(
        renderer: &mut renderer::WindowRenderer,
        models: &[AnimatedModel],
    ) -> PipelineResult<Self> {
        renderer.prepare_texture(LABEL, COLOR, 2)?;
        renderer.prepare_texture(LABEL, LINEAR, 4)?;
        for model in models {
            renderer.prepare_texture(LABEL, character_image(model), 1)?;
        }
        Self::upload(renderer, models)
    }

    pub(super) fn for_headless(
        renderer: &mut Renderer,
        models: &[AnimatedModel],
    ) -> PipelineResult<Self> {
        renderer.prepare_texture(LABEL, COLOR, 2)?;
        renderer.prepare_texture(LABEL, LINEAR, 4)?;
        for model in models {
            renderer.prepare_texture(LABEL, character_image(model), 1)?;
        }
        Self::upload(renderer, models)
    }

    fn upload(
        renderer: &mut impl AssetPreparation,
        models: &[AnimatedModel],
    ) -> PipelineResult<Self> {
        let mut images = Vec::with_capacity(6 + models.len());
        let drawing = renderer.asset_renderer();
        for format in [COLOR, LINEAR] {
            drawing.check_texture_support(format, true)?;
        }
        let mut upload =
            |desc: ImageDesc, pixels: &[u8]| -> PipelineResult<Option<MaterialTexture>> {
                let (id, image) =
                    drawing.upload_texture_mips(LABEL, desc, 0..desc.mip_levels, pixels)?;
                let start = Instant::now();
                while !drawing.poll_texture_upload(id)? {
                    if start.elapsed() > Duration::from_secs(30) {
                        return Err("lab surface upload timed out".into());
                    }
                    std::thread::yield_now();
                }
                let texture = Some(image.snapshot().clone().into());
                images.push(image);
                Ok(texture)
            };
        let mut material = |surface: SurfaceMaps| -> PipelineResult<PbrParams> {
            let mut params = PbrParams::default();
            params.factors.metallic = 1.;
            params.textures = MaterialTextures {
                base_color: upload(COLOR, surface.base)?,
                normal: upload(LINEAR, surface.normal)?,
                orm: upload(LINEAR, surface.orm)?,
                emissive: None,
            };
            Ok(params)
        };
        let floor = material(FLOOR)?;
        let steel = material(STEEL)?;
        let mut characters = Vec::with_capacity(models.len());
        for model in models {
            characters.push(PbrParams {
                textures: MaterialTextures {
                    base_color: upload(character_image(model), model.color().0)?,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        Ok(Self {
            floor,
            steel,
            characters,
            _images: images,
        })
    }
}

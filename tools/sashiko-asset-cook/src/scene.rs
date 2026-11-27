use crate::{Error, gltf::Source, store};
use sashiko_asset::{
    BlobRef, Primitive,
    scene::{Environment, EnvironmentImage, Placement, ScenePackage, SceneSettings},
};
use std::{collections::HashMap, fs, path::Path};

pub(crate) fn object(directory: &Path, bytes: &[u8]) -> Result<BlobRef, Error> {
    let reference = BlobRef::of(bytes);
    let path = directory.join("objects").join(reference.hash.to_string());
    if store::read_if_present(&path)?.as_deref() != Some(bytes) {
        store::publish(&path, bytes)?;
    }
    Ok(reference)
}

pub(super) fn cook(source: &Path, settings: &Path, directory: &Path) -> Result<(), Error> {
    #[derive(serde::Deserialize)]
    struct Recipe {
        #[serde(flatten)]
        settings: SceneSettings,
        environment: Option<Environment<std::path::PathBuf>>,
        attribution: Option<sashiko_asset::scene::Attribution>,
        #[serde(default)]
        pbrt: crate::pbrt::Options,
    }
    let recipe: Recipe = serde_json::from_slice(&fs::read(settings)?)?;
    for (name, view) in &recipe.settings.views {
        if name.trim().is_empty() {
            return Err("empty scene view name".into());
        }
        view.camera([1, 1], [0.02, 1500.])?;
    }
    let light = &recipe.settings;
    sashiko_renderer::pbr::Sun::from_lux(light.sun_direction, light.sun_color, light.sun_lux)?;
    if light.shadow_resolution < 8
        || light.shadow_distance <= 0.
        || light.sky_color.iter().any(|v| *v < 0.)
    {
        return Err("invalid scene lighting settings".into());
    }
    let environment = recipe
        .environment
        .map(|env| -> Result<_, Error> {
            let load = |image: EnvironmentImage<std::path::PathBuf>| -> Result<_, Error> {
                let data = fs::read(
                    settings
                        .parent()
                        .ok_or("missing recipe directory")?
                        .join(image.data),
                )?;
                let image = EnvironmentImage {
                    width: image.width,
                    height: image.height,
                    mip_levels: image.mip_levels,
                    data: object(directory, &data)?,
                };
                image.validate()?;
                Ok(image)
            };
            let [a, b, c] = env.maps;
            Ok(Environment {
                maps: [load(a)?, load(b)?, load(c)?],
                nits_per_unit: env.nits_per_unit,
                yaw_radians: env.yaw_radians,
            })
        })
        .transpose()?;
    let source = source.canonicalize()?;
    let mut scene = ScenePackage {
        version: 1,
        profile: sashiko_asset::scene::SceneProfile::PbrRgba8Rgba16f,
        settings: recipe.settings,
        environment,
        primitives: Vec::new(),
        placements: Vec::new(),
        physical: None,
        attribution: recipe.attribution,
    };
    if source
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pbrt"))
    {
        crate::pbrt::cook(&source, directory, recipe.pbrt, &mut scene)?;
    } else {
        let src = Source::open(&source, source.parent().ok_or("missing source directory")?)?;
        cook_gltf(src, directory, &mut scene)?;
    }
    scene.validate()?;
    store::publish(&directory.join("scene.json"), &serde_json::to_vec(&scene)?)?;
    Ok(())
}

fn cook_gltf(src: Source, directory: &Path, scene: &mut ScenePackage) -> Result<(), Error> {
    scene.placements.reserve(src.len());
    let mut primitive_ids = HashMap::new();
    let mut textures = HashMap::new();
    for i in 0..src.len() {
        let key = src.local_primitive(i);
        let index = match primitive_ids.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let mut images = [None; 4];
                for (slot, image) in src.images(i).into_iter().enumerate() {
                    if let Some((index, role)) = image {
                        let reference = match textures.entry((index, role)) {
                            std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                            std::collections::hash_map::Entry::Vacant(e) => {
                                *e.insert(object(directory, &src.cook_image(index, role)?)?)
                            }
                        };
                        images[slot] = Some(reference);
                    }
                }
                let index = scene.primitives.len();
                scene.primitives.push(Primitive {
                    mesh: object(directory, &src.cook_local_mesh(i)?)?,
                    material: object(directory, &src.cook_material(i)?)?,
                    images,
                });
                *entry.insert(index)
            }
        };
        scene.placements.push(Placement {
            primitive: index,
            transform: src.placement(i),
        });
    }
    Ok(())
}

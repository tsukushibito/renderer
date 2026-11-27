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

#[derive(serde::Deserialize)]
struct ReducedGltf {
    path: std::path::PathBuf,
    min_pixels: f32,
    #[serde(default)]
    material_min_pixels: HashMap<usize, f32>,
}

pub(super) fn cook(source: &Path, settings: &Path, directory: &Path) -> Result<(), Error> {
    #[derive(serde::Deserialize)]
    struct Recipe {
        #[serde(flatten)]
        settings: SceneSettings,
        environment: Option<Environment<std::path::PathBuf>>,
        attribution: Option<sashiko_asset::scene::Attribution>,
        backdrop: Option<EnvironmentImage<std::path::PathBuf>>,
        reduced_gltf: Option<ReducedGltf>,
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
        backdrop: None,
        mesh_lods: Vec::new(),
        attribution: recipe.attribution,
    };
    if source
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pbrt"))
    {
        crate::pbrt::cook(&source, directory, recipe.pbrt, &mut scene)?;
    } else {
        let src = Source::open(&source, source.parent().ok_or("missing source directory")?)?;
        let alternate = recipe
            .reduced_gltf
            .map(|lod| -> Result<_, Error> {
                let path = settings
                    .parent()
                    .ok_or("missing recipe directory")?
                    .join(&lod.path)
                    .canonicalize()?;
                if std::iter::once(&lod.min_pixels)
                    .chain(lod.material_min_pixels.values())
                    .any(|size| !size.is_finite() || *size <= 0.)
                {
                    return Err("invalid LOD size".into());
                }
                Ok((
                    Source::open(
                        &path,
                        path.parent().ok_or("missing reduced source directory")?,
                    )?,
                    lod,
                ))
            })
            .transpose()?;
        cook_gltf(src, alternate, directory, &mut scene)?;
        if let Some(map) = recipe.backdrop {
            let data = fs::read(
                settings
                    .parent()
                    .ok_or("missing recipe directory")?
                    .join(map.data),
            )?;
            let sky = sashiko_asset::scene::InfiniteSky {
                equal_area_radiance: EnvironmentImage {
                    width: map.width,
                    height: map.height,
                    mip_levels: map.mip_levels,
                    data: object(directory, &data)?,
                },
                world_from_light: glam::Mat4::IDENTITY.to_cols_array_2d(),
                scale: 1.,
            };
            sky.equal_area_radiance.validate()?;
            if map.width != map.height || map.mip_levels != 1 {
                return Err("expected square equal-area sky".into());
            }
            if scene.environment.is_none() {
                scene.environment = Some(crate::pbrt::lighting::cook(directory, &sky)?);
            }
            scene.backdrop = Some(sky);
        }
    }
    scene.validate()?;
    store::publish(&directory.join("scene.json"), &serde_json::to_vec(&scene)?)?;
    Ok(())
}

fn cook_gltf(
    src: Source,
    alternate: Option<(Source, ReducedGltf)>,
    directory: &Path,
    scene: &mut ScenePackage,
) -> Result<(), Error> {
    if alternate
        .as_ref()
        .is_some_and(|(s, _)| s.len() != src.len())
    {
        return Err("LOD source placements differ".into());
    }
    let mut alternates = HashMap::new();
    scene.placements.reserve(src.len());
    let mut primitive_ids = HashMap::new();
    let mut textures = HashMap::new();
    for i in 0..src.len() {
        let key = src.local_primitive(i);
        if alternate
            .as_ref()
            .is_some_and(|(s, _)| s.local_primitive(i) != key || s.placement(i) != src.placement(i))
        {
            return Err("LOD source placement identity differs".into());
        }
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
                let mesh = object(directory, &src.cook_local_mesh(i)?)?;
                if let Some((reduced, recipe)) = &alternate {
                    let low = object(directory, &reduced.cook_local_mesh(i)?)?;
                    let size = src
                        .material_index(i)
                        .and_then(|id| recipe.material_min_pixels.get(&id))
                        .copied()
                        .unwrap_or(recipe.min_pixels);
                    let (previous, min_pixels) = alternates.entry(mesh).or_insert((low, size));
                    if *previous != low {
                        return Err("conflicting reduced geometry for shared mesh".into());
                    }
                    *min_pixels = min_pixels.min(size);
                }
                scene.primitives.push(Primitive {
                    mesh,
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
    if alternate.is_some() {
        // The runtime imports one mesh per adjacent run. Preserve placement order
        // inside each primitive group so repeats share the same graph import.
        scene
            .placements
            .sort_by_key(|placement| placement.primitive);
        scene.mesh_lods = alternates
            .into_iter()
            .filter(|(source, (reduced, _))| source != reduced)
            .map(
                |(source, (reduced, min_pixels))| sashiko_asset::scene::MeshLod {
                    source,
                    reduced,
                    min_pixels,
                },
            )
            .collect();
        scene
            .mesh_lods
            .sort_by_key(|lod| lod.source.hash.to_string());
    }
    Ok(())
}

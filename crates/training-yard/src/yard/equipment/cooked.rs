//! Convert verified cooked stream objects on the equipment reader thread.
use renderer::{
    core::{graph::ImageDesc, program::SamplerDescriptor},
    material::{MaterialTextures, PbrParams},
    mesh::MeshData,
    pbr::PbrVertex,
};
use renderer_asset::{AssetId, Digest, Generation, ObjectBytes, ObjectStore, Primitive};
use std::{io, sync::Arc};

pub(super) struct Map {
    pub hash: Digest,
    pub object: Arc<ObjectBytes>,
    pub desc: ImageDesc,
}

pub(super) struct LoadedPart {
    pub mesh: MeshData,
    pub params: PbrParams,
    pub primitive: Primitive,
    pub maps: [Option<Map>; 4],
    // Keep the weak object cache live for other users of this revision.
    pub sources: [Arc<ObjectBytes>; 2],
}

pub(super) fn read(
    generation: &Generation,
    store: &ObjectStore,
    id: AssetId,
) -> Result<Vec<LoadedPart>, String> {
    read_inner(generation, store, id).map_err(|error| format!("equipment {id}: {error}"))
}

fn read_inner(
    generation: &Generation,
    store: &ObjectStore,
    id: AssetId,
) -> Result<Vec<LoadedPart>, Box<dyn std::error::Error>> {
    let recipe = generation.equipment(&id)?;
    if recipe.primitives.is_empty() || recipe.primitives.len() > super::MAX_PARTS {
        return Err(format!(
            "armor requires between 1 and {} primitives",
            super::MAX_PARTS
        )
        .into());
    }
    recipe
        .primitives
        .iter()
        .map(|p| read_part(store, p))
        .collect()
}

fn read_part(
    store: &ObjectStore,
    primitive: &Primitive,
) -> Result<LoadedPart, Box<dyn std::error::Error>> {
    let mesh_obj = store.load(&primitive.mesh)?;
    let mesh = mesh_obj.mesh()?;
    let vertices = mesh
        .vertices
        .iter()
        .map(|v| {
            PbrVertex::new(
                [v[0], v[1], v[2]],
                [v[3], v[4], v[5]],
                [v[6], v[7], v[8], v[9]],
                [v[10], v[11]],
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mesh = MeshData::pbr(&vertices, &mesh.indices)?;
    let mat_obj = store.load(&primitive.material)?;
    let material = mat_obj.material()?;
    let factors = material.factors()?;
    let mut samplers = [SamplerDescriptor::LinearRepeat; 4];
    let mut maps = [None, None, None, None];
    for (i, image_ref) in primitive.images.iter().enumerate() {
        if let Some(s) = &material.samplers[i] {
            samplers[i] = s.descriptor()?;
        }
        if let Some(reference) = image_ref {
            let object = store.load(reference)?;
            let image = object.image()?;
            let expects_srgb = i == 0 || i == 3;
            if expects_srgb != image.srgb {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "image color space does not match material slot",
                )
                .into());
            }
            let mut desc = if image.srgb {
                ImageDesc::rgba8_srgb(image.width, image.height)
            } else {
                ImageDesc::rgba8(image.width, image.height)
            };
            desc.mip_levels = image.mip_levels;
            maps[i] = Some(Map {
                hash: reference.hash,
                object,
                desc,
            });
        }
    }
    let [base_color, normal, orm, emissive] = samplers;
    Ok(LoadedPart {
        mesh,
        params: PbrParams {
            factors,
            samplers: MaterialTextures {
                base_color,
                normal,
                orm,
                emissive,
            },
            ..Default::default()
        },
        primitive: primitive.clone(),
        maps,
        sources: [mesh_obj, mat_obj],
    })
}

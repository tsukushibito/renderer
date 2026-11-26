//! Convert verified cooked stream objects on the equipment reader thread.
use sashiko_asset::{
    AssetId, Digest, Generation, MaterialSlot, ObjectBytes, ObjectStore, Primitive,
};
use sashiko_renderer::{core::graph::ImageDesc, material::PbrParams, mesh::MeshData};
use std::sync::Arc;

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
    if recipe.primitives.len() > super::MAX_PARTS {
        return Err(format!("armor exceeds its limit of {} primitives", super::MAX_PARTS).into());
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
    let mesh = mesh_obj.mesh()?.mesh_data()?;
    let mat_obj = store.load(&primitive.material)?;
    let material = mat_obj.material()?;
    let params = material.parameters()?;
    let mut maps = [None, None, None, None];
    for slot in MaterialSlot::ALL {
        let i = slot.index();
        if let Some(reference) = &primitive.images[i] {
            let object = store.load(reference)?;
            let desc = object.image()?.texture_desc(slot)?;
            maps[i] = Some(Map {
                hash: reference.hash,
                object,
                desc,
            });
        }
    }
    Ok(LoadedPart {
        mesh,
        params,
        primitive: primitive.clone(),
        maps,
        sources: [mesh_obj, mat_obj],
    })
}

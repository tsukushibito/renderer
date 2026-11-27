use super::super::{Result, geometry::MeshRange};
use sashiko_asset::{ObjectStore, scene::ScenePackage};
use std::collections::HashMap;

/// Byte offsets in the immutable shader buffer. A visibility ray only needs
/// positions, UVs, indices and the source opacity image.
pub(super) struct World {
    pub bytes: Vec<u8>,
    pub meshes: Vec<MeshRange>,
    pub parts: Vec<usize>,
}

pub(super) fn pack(scene: &ScenePackage, store: &ObjectStore, limit: u64) -> Result<World> {
    let physical = scene
        .physical
        .as_ref()
        .ok_or("film tracing needs physical surfaces")?;
    let header = scene
        .primitives
        .len()
        .checked_mul(12)
        .ok_or("film table overflow")?;
    if header as u64 > limit || header > u32::MAX as usize {
        return Err("film primitive table exceeds device limits".into());
    }
    let mut bytes = vec![0; header];
    let mut meshes: Vec<MeshRange> = Vec::new();
    let mut by_mesh = HashMap::new();
    let mut textures = HashMap::new();
    let mut parts = Vec::with_capacity(scene.primitives.len());
    let offset = |v: &[u8]| -> Result<u32> {
        if v.len() as u64 > limit {
            return Err("scene exceeds the device storage-buffer range".into());
        }
        Ok(u32::try_from(v.len())?)
    };
    for (i, (part, surface)) in scene.primitives.iter().zip(&physical.surfaces).enumerate() {
        let mesh_id = match by_mesh.entry(part.mesh) {
            std::collections::hash_map::Entry::Occupied(e) => *e.get(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let obj = store.load(&part.mesh)?;
                let mesh = obj.mesh()?;
                let vertices = offset(&bytes)?;
                for v in &mesh.vertices {
                    for x in v.position.into_iter().chain(v.uv) {
                        bytes.extend(x.to_le_bytes());
                    }
                }
                let indices = offset(&bytes)?;
                for index in &mesh.indices {
                    bytes.extend(index.to_le_bytes());
                }
                let id = meshes.len();
                meshes.push(MeshRange {
                    vertices,
                    indices,
                    vertex_count: u32::try_from(mesh.vertices.len())?,
                    triangles: u32::try_from(mesh.indices.len() / 3)?,
                });
                *e.insert(id)
            }
        };
        parts.push(mesh_id);
        let mask = if let Some(reference) = surface.alpha {
            if let Some(&at) = textures.get(&reference) {
                at
            } else {
                let obj = store.load(&reference)?;
                let blob = obj.image()?;
                if blob.srgb {
                    return Err("film opacity must use linear scalar samples".into());
                }
                let at = offset(&bytes)?;
                bytes.extend(blob.width.to_le_bytes());
                bytes.extend(blob.height.to_le_bytes());
                let size = usize::try_from(u64::from(blob.width) * u64::from(blob.height) * 4)?;
                bytes.extend_from_slice(&blob.pixels[..size]);
                textures.insert(reference, at);
                at
            }
        } else {
            0
        };
        let mesh = &meshes[mesh_id];
        let words = [mesh.vertices, mesh.indices, mask];
        for (dst, word) in bytes[i * 12..(i + 1) * 12]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(words)
        {
            *dst = word.to_le_bytes();
        }
    }
    offset(&bytes)?;
    Ok(World {
        bytes,
        meshes,
        parts,
    })
}

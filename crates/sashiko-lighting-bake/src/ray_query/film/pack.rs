use super::super::{Result, geometry::MeshRange};
use sashiko_asset::{
    BlobRef, ObjectStore,
    scene::{Scattering, ScenePackage},
};
use std::collections::HashMap;

// Cosine-weighted fraction reflected back into a diffuse substrate. Integrate
// once per material; include total internal reflection above the critical angle.
fn internal_reflectance(eta: f32) -> f32 {
    let mut sum = 0.;
    for i in 0..128 {
        let cos_i = ((i as f32 + 0.5) / 128.).sqrt();
        let sin2_t = eta * eta * (1. - cos_i * cos_i);
        sum += if sin2_t >= 1. {
            1.
        } else {
            let cos_t = (1. - sin2_t).sqrt();
            let s = (eta * cos_i - cos_t) / (eta * cos_i + cos_t);
            let p = (cos_i - eta * cos_t) / (cos_i + eta * cos_t);
            (s * s + p * p) * 0.5
        };
    }
    sum / 128.
}

/// Byte offsets in the immutable shader buffer. A visibility ray only needs
/// positions, UVs, indices and the source opacity image.
pub(super) struct World {
    pub bytes: Vec<u8>,
    pub meshes: Vec<MeshRange>,
    pub parts: Vec<usize>,
}

pub(super) fn pack(
    scene: &ScenePackage,
    store: &ObjectStore,
    limit: u64,
    shaded: bool,
) -> Result<World> {
    let physical = scene
        .physical
        .as_ref()
        .ok_or("film tracing needs physical surfaces")?;
    let (base, stride) = if shaded { (128, 64) } else { (0, 12) };
    let header = scene
        .primitives
        .len()
        .checked_mul(stride)
        .and_then(|n| n.checked_add(base))
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
                    if shaded {
                        for x in v.normal {
                            bytes.extend(x.to_le_bytes());
                        }
                    }
                }
                let indices = offset(&bytes)?;
                for index in &mesh.indices {
                    bytes.extend(index.to_le_bytes());
                }
                let id = meshes.len();
                meshes.push(MeshRange {
                    vertices,
                    vertex_stride: if shaded { 32 } else { 20 },
                    indices,
                    vertex_count: u32::try_from(mesh.vertices.len())?,
                    triangles: u32::try_from(mesh.indices.len() / 3)?,
                });
                *e.insert(id)
            }
        };
        parts.push(mesh_id);
        let mut image = |reference: Option<BlobRef>, srgb: bool| -> Result<u32> {
            let Some(reference) = reference else {
                return Ok(0);
            };
            if let Some(&at) = textures.get(&(reference, srgb)) {
                return Ok(at);
            }
            let obj = store.load(&reference)?;
            let blob = obj.image()?;
            if blob.srgb != srgb {
                return Err("incorrect transfer function for film material texture".into());
            }
            let at = offset(&bytes)?;
            bytes.extend(blob.width.to_le_bytes());
            bytes.extend(blob.height.to_le_bytes());
            let size = usize::try_from(u64::from(blob.width) * u64::from(blob.height) * 4)?;
            bytes.extend_from_slice(&blob.pixels[..size]);
            textures.insert((reference, srgb), at);
            Ok(at)
        };
        let mesh = &meshes[mesh_id];
        let mut words = [0u32; 16];
        words[..3].copy_from_slice(&[mesh.vertices, mesh.indices, image(surface.alpha, false)?]);
        if shaded {
            let kind = match surface.scattering {
                Scattering::Diffuse => 0,
                Scattering::DiffuseTransmission => 1,
                Scattering::CoatedDiffuse => 2,
                Scattering::Conductor => 3,
                Scattering::Dielectric => 4,
            };
            let [r, g, b] = surface.reflectance.map(f32::to_bits);
            words[3..13].copy_from_slice(&[
                image(surface.color, true)?,
                image(surface.transmittance, true)?,
                image(surface.displacement, false)?,
                kind,
                surface.eta.to_bits(),
                r,
                g,
                b,
                surface.roughness.to_bits(),
                surface.scale.to_bits(),
            ]);
            if kind == 2 {
                words[13] = internal_reflectance(surface.eta).to_bits();
            }
        }
        for (dst, word) in bytes[base + i * stride..base + (i + 1) * stride]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(words)
        {
            *dst = word.to_le_bytes();
        }
    }
    if shaded {
        if physical.surfaces.iter().any(|s| s.displacement.is_some()) {
            let unit = physical
                .displacement_unit_metres
                .ok_or("recook the scene to retain physical displacement units")?;
            bytes[20..24].copy_from_slice(&unit.to_le_bytes());
        }
        super::sky::append(
            &mut bytes,
            store,
            physical
                .sky
                .as_ref()
                .ok_or("film transport requires an infinite light")?,
            limit,
        )?;
    }
    offset(&bytes)?;
    Ok(World {
        bytes,
        meshes,
        parts,
    })
}

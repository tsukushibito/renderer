//! Size the stream at a change boundary from the requested and still-live objects.
use super::{IMAGE_BUDGET_BYTES, ITEMS, LoadedPart, MAX_PARTS};
use renderer::core::{
    graph::{BufferDesc, ImageDesc},
    program::SamplerDescriptor,
};
use renderer_asset::Digest;

#[derive(Default)]
pub(super) struct Requirements {
    meshes: Vec<(Digest, [BufferDesc; 2])>,
    images: Vec<(Digest, ImageDesc)>,
    samplers: Vec<SamplerDescriptor>,
}

#[derive(Default, PartialEq)]
pub(super) struct Reservations {
    pub buffers: Vec<BufferDesc>,
    pub images: Vec<ImageDesc>,
    pub samplers: Vec<SamplerDescriptor>,
}

pub(super) const QUAD_VERTEX_BYTES: u64 = 4 * renderer::passes::pbr::VERTEX_BYTES as u64;
pub(super) const QUAD_INDEX_BYTES: u64 = 6 * 4;

fn push_unique<T: PartialEq>(items: &mut Vec<T>, value: T) {
    if !items.contains(&value) {
        items.push(value);
    }
}

impl Requirements {
    pub fn for_parts(parts: &[LoadedPart]) -> Result<Self, String> {
        let mut needed = Self::default();
        for part in parts {
            let mesh = part.sources[0].mesh().map_err(|e| e.to_string())?;
            let vertex_bytes = mesh.vertices.len() * renderer::passes::pbr::VERTEX_BYTES;
            let index_bytes = std::mem::size_of_val(mesh.indices.as_slice());
            push_unique(
                &mut needed.meshes,
                (
                    part.primitive.mesh.hash,
                    [
                        BufferDesc {
                            size: vertex_bytes as u64,
                        },
                        BufferDesc {
                            size: index_bytes as u64,
                        },
                    ],
                ),
            );
            let samplers = &part.params.samplers;
            for (map, sampler) in part.maps.iter().zip([
                samplers.base_color,
                samplers.normal,
                samplers.orm,
                samplers.emissive,
            ]) {
                if let Some(map) = map {
                    if map
                        .desc
                        .byte_len()
                        .is_none_or(|n| n > IMAGE_BUDGET_BYTES as u64)
                    {
                        return Err(format!(
                            "image transfer exceeds the training-yard budget of {IMAGE_BUDGET_BYTES} bytes"
                        ));
                    }
                    push_unique(&mut needed.images, (map.hash, map.desc));
                    if sampler != SamplerDescriptor::LinearRepeat {
                        push_unique(&mut needed.samplers, sampler);
                    }
                }
            }
        }
        Ok(needed)
    }
}

impl Reservations {
    pub fn for_items<'a>(items: impl Iterator<Item = &'a Requirements>) -> Self {
        let mut all = Requirements::default();
        for item in items {
            for &mesh in &item.meshes {
                push_unique(&mut all.meshes, mesh);
            }
            for &image in &item.images {
                push_unique(&mut all.images, image);
            }
            for &sampler in &item.samplers {
                push_unique(&mut all.samplers, sampler);
            }
        }
        let mut extra = Self {
            samplers: all.samplers,
            ..Default::default()
        };
        let mut quad_vertices = ITEMS * MAX_PARTS;
        let mut quad_indices = ITEMS * MAX_PARTS;
        for (_, buffers) in all.meshes {
            for buffer in buffers {
                match buffer.size {
                    QUAD_VERTEX_BYTES if quad_vertices != 0 => quad_vertices -= 1,
                    QUAD_INDEX_BYTES if quad_indices != 0 => quad_indices -= 1,
                    _ => extra.buffers.push(buffer),
                }
            }
        }
        let mut swatches = ITEMS;
        for (_, image) in all.images {
            if image == ImageDesc::rgba8_srgb(1, 1) && swatches != 0 {
                swatches -= 1;
            } else {
                extra.images.push(image);
            }
        }
        extra
    }
}

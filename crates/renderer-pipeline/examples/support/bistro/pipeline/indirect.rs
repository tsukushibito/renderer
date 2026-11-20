//! Packed opaque geometry and fixed native indirect command slots.

use super::super::{Error, asset::Scene, camera::Frustum, program, shadow::Bounds};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::{upload_buffer, upload_geometry};
use program::visibility::{VISIBILITY_AABB_BYTES, VISIBILITY_MASK_BYTES, VISIBILITY_THREADS};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::graph::GraphPool;
use renderer::graph::{
    BufferHandle, Graph, GraphError, ImportBuffer, IndexedIndirectTable, IndexedIndirectTemplate,
    RasterConstants, RasterDraw,
};
use renderer_pipeline::{DrawCandidate, FrameIndirectGroup, IndirectTable};
use std::sync::Arc;

struct Draw {
    primitive: usize,
    range: IndexedIndirectTemplate,
}

struct Group {
    material: usize,
    draws: Vec<Draw>,
    table: IndexedIndirectTable,
    mask_offset: u32,
}

impl Group {
    fn visible_draws<'a>(
        &'a self,
        frustum: &'a Frustum,
        bounds: &'a [Bounds],
    ) -> impl Iterator<Item = &'a Draw> {
        self.draws
            .iter()
            .filter(|draw| frustum.overlaps(bounds[draw.primitive]))
    }
}

pub(in super::super) struct Layout {
    groups: Vec<Group>,
    pub vertex_bytes: usize,
    pub index_count: u32,
    padded_count: u32,
}

impl Layout {
    pub fn new(scene: &Scene) -> Result<Option<Self>, Error> {
        let mut by_material = vec![Vec::new(); scene.materials.len()];
        for (index, primitive) in scene.primitives.iter().enumerate() {
            if !scene.materials[primitive.material].blend {
                by_material[primitive.material].push(index);
            }
        }
        let mut groups = Vec::new();
        let mut vertex_bytes = 0usize;
        let mut index_count = 0u32;
        let mut padded_count = 0u32;
        for (material, members) in by_material.into_iter().enumerate() {
            if members.is_empty() {
                continue;
            }
            let mut draws = Vec::with_capacity(members.len());
            for primitive in members {
                let mesh = &scene.primitives[primitive];
                vertex_bytes = vertex_bytes
                    .checked_add(mesh.vertices.len())
                    .ok_or("packed vertex size overflow")?;
                let range = IndexedIndirectTemplate {
                    first_index: index_count,
                    index_count: mesh.index_count,
                };
                index_count = index_count
                    .checked_add(mesh.index_count)
                    .ok_or("packed index count overflow")?;
                draws.push(Draw { primitive, range });
            }
            let templates = draws.iter().map(|draw| draw.range).collect::<Vec<_>>();
            let table = IndexedIndirectTable::new(&templates)?;
            let next_offset = padded_count
                .checked_add(table.padded_count())
                .filter(|count| count / VISIBILITY_THREADS <= 65_535)
                .ok_or("packed visibility exceeds the supported dispatch count")?;
            groups.push(Group {
                material,
                draws,
                table,
                mask_offset: padded_count,
            });
            padded_count = next_offset;
        }
        let _ = usize::try_from(u64::from(index_count) * size_of::<u32>() as u64)?;
        Ok((!groups.is_empty()).then_some(Self {
            groups,
            vertex_bytes,
            index_count,
            padded_count,
        }))
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn draw_count(&self) -> usize {
        self.groups.iter().map(|group| group.draws.len()).sum()
    }

    pub fn index_bytes(&self) -> usize {
        self.index_count as usize * size_of::<u32>()
    }

    pub fn aabb_bytes(&self) -> usize {
        self.padded_count as usize * VISIBILITY_AABB_BYTES
    }

    pub fn mask_bytes(&self) -> u64 {
        u64::from(self.padded_count) * VISIBILITY_MASK_BYTES as u64
    }

    pub fn dispatch_groups(&self) -> u32 {
        self.padded_count / VISIBILITY_THREADS
    }

    pub fn indirect_tables(&self) -> impl ExactSizeIterator<Item = IndirectTable> + '_ {
        self.groups.iter().map(|group| IndirectTable {
            table: group.table.clone(),
            visibility_offset: u64::from(group.mask_offset) * VISIBILITY_MASK_BYTES as u64,
        })
    }

    pub fn template_upload_bytes(&self) -> usize {
        self.groups
            .iter()
            .map(|group| group.table.template_buffer_desc().size as usize)
            .sum()
    }

    fn pack(&self, scene: &Scene) -> Result<(Vec<u8>, Vec<u32>), Error> {
        let mut vertices = Vec::with_capacity(self.vertex_bytes);
        let mut indices = Vec::with_capacity(self.index_count as usize);
        for draw in self.groups.iter().flat_map(|group| &group.draws) {
            let mesh = &scene.primitives[draw.primitive];
            let start = u32::try_from(vertices.len() / program::VERTEX_BYTES)?;
            vertices.extend_from_slice(&scene.vertices[mesh.vertices.clone()]);
            for bytes in scene.indices[mesh.indices.clone()].as_chunks::<4>().0 {
                indices.push(
                    start
                        .checked_add(u32::from_le_bytes(*bytes))
                        .ok_or("packed vertex index overflow")?,
                );
            }
        }
        Ok((vertices, indices))
    }

    fn aabbs(&self, bounds: &[Bounds]) -> Vec<u8> {
        let mut bytes = vec![0; self.aabb_bytes()];
        for group in &self.groups {
            let offset = group.mask_offset as usize;
            for (index, draw) in group.draws.iter().enumerate() {
                let item = bounds[draw.primitive];
                let start = (offset + index) * VISIBILITY_AABB_BYTES;
                let words = bytes[start..start + VISIBILITY_AABB_BYTES]
                    .as_chunks_mut::<4>()
                    .0;
                for (destination, value) in words[..3].iter_mut().zip(item.min) {
                    *destination = value.to_le_bytes();
                }
                words[3] = 1u32.to_le_bytes();
                for (destination, value) in words[4..7].iter_mut().zip(item.max) {
                    *destination = value.to_le_bytes();
                }
            }
        }
        bytes
    }
}

pub(super) struct Resident {
    layout: Arc<Layout>,
    vertices: ImportBuffer,
    indices: ImportBuffer,
    aabbs: Option<ImportBuffer>,
}

impl Resident {
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn upload(
        backend: &mut renderer::runtime::Runtime,
        geometry_pool: &mut GraphPool,
        upload_owners: &mut Vec<GraphPool>,
        scene: &Scene,
        layout: &Arc<Layout>,
        bounds: &[Bounds],
        indirect: bool,
    ) -> Result<Self, Error> {
        let (vertices, indices) = layout.pack(scene)?;
        let (vertices, indices) = upload_geometry(
            backend,
            geometry_pool,
            program::VERTEX_BYTES as u32,
            &vertices,
            &indices,
        )?;
        let aabbs = if indirect {
            Some(upload_buffer(
                backend,
                upload_owners,
                &layout.aabbs(bounds),
            )?)
        } else {
            None
        };
        Ok(Self {
            vertices,
            indices,
            aabbs,
            layout: Arc::clone(layout),
        })
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub fn release(self) -> Released {
        Released {
            layout: self.layout,
            vertices: self.vertices.key(),
            indices: self.indices.key(),
            aabbs: self.aabbs.as_ref().map(ImportBuffer::key),
        }
    }

    pub fn import_frame<'a>(&'a self, graph: &mut Graph) -> Result<Frame<'a>, GraphError> {
        let vertices = graph.import_buffer("packed opaque vertices", &self.vertices)?;
        let indices = graph.import_buffer("packed opaque indices", &self.indices)?;
        let bounds = self
            .aabbs
            .as_ref()
            .map(|bounds| graph.import_buffer("packed opaque bounds", bounds))
            .transpose()?;
        Ok(Frame {
            layout: &self.layout,
            vertices,
            indices,
            bounds,
        })
    }
}

#[cfg(all(windows, feature = "presentation"))]
pub(super) struct Released {
    layout: Arc<Layout>,
    vertices: renderer::graph::ResourceKey,
    indices: renderer::graph::ResourceKey,
    aabbs: Option<renderer::graph::ResourceKey>,
}

#[cfg(all(windows, feature = "presentation"))]
impl Released {
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn refresh(self, backend: &renderer::runtime::Runtime) -> Result<Resident, Error> {
        Ok(Resident {
            layout: self.layout,
            vertices: backend.refresh_buffer(self.vertices)?,
            indices: backend.refresh_buffer(self.indices)?,
            aabbs: self
                .aabbs
                .map(|key| backend.refresh_buffer(key))
                .transpose()?,
        })
    }
}

pub(super) struct Frame<'a> {
    layout: &'a Layout,
    vertices: BufferHandle,
    indices: BufferHandle,
    pub bounds: Option<BufferHandle>,
}

impl Frame<'_> {
    pub fn indirect_groups<'a>(
        &'a self,
        include_material: impl Fn(usize) -> bool + 'a,
    ) -> impl Iterator<Item = FrameIndirectGroup> + 'a {
        self.layout
            .groups
            .iter()
            .map(move |group| FrameIndirectGroup {
                vertices: self.vertices,
                indices: self.indices,
                material: Some(group.material),
                enabled: include_material(group.material),
            })
    }

    pub fn direct_candidates<'a>(
        &'a self,
        frustum: &'a Frustum,
        bounds: &'a [Bounds],
        include_material: impl Fn(usize) -> bool + 'a,
    ) -> impl Iterator<Item = DrawCandidate<'static>> + 'a {
        self.layout
            .groups
            .iter()
            .filter(move |group| include_material(group.material))
            .flat_map(move |group| {
                group
                    .visible_draws(frustum, bounds)
                    .map(move |draw| DrawCandidate {
                        queue: "opaque",
                        material: Some(group.material),
                        draw: RasterDraw {
                            bindings: None,
                            vertices: self.vertices,
                            indices: self.indices,
                            first_index: draw.range.first_index,
                            index_count: draw.range.index_count,
                            instance_count: 1,
                            parameters: RasterConstants::default(),
                        },
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_ranges_keep_material_order_and_mask_offsets() {
        let mut scene = Scene::fixture(false);
        // Exercise more than one primitive in the first material batch.
        scene.primitives[1].material = 0;
        let layout = Layout::new(&scene).unwrap().unwrap();
        assert!(
            layout
                .groups
                .iter()
                .map(|group| group.material)
                .eq([0, 2, 3, 4, 6])
        );
        let first = &layout.groups[0];
        assert_eq!(first.mask_offset, 0);
        assert_eq!(first.table.command_count(), 2);
        assert_eq!(first.draws[1].range.first_index, 6);
        assert_eq!(layout.groups[1].mask_offset, VISIBILITY_THREADS);
        assert_eq!(layout.groups[1].draws[0].range.first_index, 12);

        let (vertices, indices) = layout.pack(&scene).unwrap();
        assert_eq!(vertices.len(), 6 * 4 * program::VERTEX_BYTES);
        assert_eq!(&indices[..12], &[0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]);
        assert_eq!(&indices[12..18], &[8, 9, 10, 8, 10, 11]);

        let bounds = (0..scene.primitives.len())
            .map(|index| Bounds {
                min: [index as f32, 1.0, 2.0],
                max: [index as f32 + 1.0, 2.0, 3.0],
            })
            .collect::<Vec<_>>();
        let records = layout.aabbs(&bounds);
        assert_eq!(
            records.len(),
            5 * VISIBILITY_THREADS as usize * VISIBILITY_AABB_BYTES
        );
        let (records, tail) = records.as_chunks::<VISIBILITY_AABB_BYTES>();
        assert!(tail.is_empty());
        assert_eq!(&records[1][0..4], &1.0f32.to_le_bytes());
        assert_eq!(&records[1][12..16], &1u32.to_le_bytes());
        assert_eq!(records[2], [0; VISIBILITY_AABB_BYTES]);
        let second_group = VISIBILITY_THREADS as usize;
        assert_eq!(&records[second_group][0..4], &2.0f32.to_le_bytes());
        assert_eq!(&records[second_group][12..16], &1u32.to_le_bytes());
    }

    #[test]
    fn prepared_indirect_tables_use_the_packed_visibility_offsets() {
        let scene = Scene::fixture(false);
        let layout = Layout::new(&scene).unwrap().unwrap();
        let tables = layout.indirect_tables().collect::<Vec<_>>();
        assert_eq!(tables.len(), layout.group_count());
        assert_eq!(layout.dispatch_groups() as usize, tables.len());
        assert!(tables.iter().enumerate().all(|(index, group)| {
            group.visibility_offset
                == (index * VISIBILITY_THREADS as usize * VISIBILITY_MASK_BYTES) as u64
        }));
    }
}

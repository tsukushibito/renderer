//! Packed opaque geometry and fixed native indirect command slots.

use super::super::{
    Error, Options, SubmissionMode, asset::Scene, backend::Backend, camera::Frustum, program,
    shadow::Bounds,
};
use super::{upload_buffer, upload_geometry};
use program::visibility::{
    VISIBILITY_AABB_BYTES, VISIBILITY_CAMERA_BYTES, VISIBILITY_MASK_BYTES, VISIBILITY_THREADS,
    visibility_program,
};
use renderer::{
    graph::{
        Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError, GraphPool,
        ImportBuffer, IndexedIndirectArgs, IndexedIndirectTable, IndexedIndirectTemplate,
        RasterBindings, RasterConstants, RasterDraw, RasterPass,
    },
    program::ManagedRasterProgram,
};
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

pub(super) struct Layout {
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

    fn mask_bytes(&self) -> u64 {
        u64::from(self.padded_count) * VISIBILITY_MASK_BYTES as u64
    }

    pub fn template_upload_bytes(&self) -> usize {
        self.groups
            .iter()
            .map(|group| group.table.template_buffer_desc().size as usize)
            .sum()
    }

    pub fn transient_buffers(&self, options: &Options) -> impl Iterator<Item = (u64, usize)> + '_ {
        let indirect = options.submission == SubmissionMode::Indirect;
        let inflight = options.inflight;
        indirect
            .then_some((self.mask_bytes(), inflight))
            .into_iter()
            .chain(
                self.groups
                    .iter()
                    .filter(move |_| indirect)
                    .flat_map(move |group| {
                        [
                            (group.table.template_buffer_desc().size, inflight),
                            (group.table.command_buffer_desc().size, inflight),
                        ]
                    }),
            )
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
    pub fn upload<B: Backend>(
        backend: &mut B,
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

    pub fn prepare_frame<'a>(
        &'a self,
        graph: &mut Graph,
        camera: BufferHandle,
        commands: &'a mut Vec<IndexedIndirectArgs>,
    ) -> Result<Frame<'a>, GraphError> {
        let vertices = graph.import_buffer("packed opaque vertices", &self.vertices)?;
        let indices = graph.import_buffer("packed opaque indices", &self.indices)?;
        commands.clear();
        if let Some(aabbs) = &self.aabbs {
            let aabbs = graph.import_buffer("packed opaque bounds", aabbs)?;
            let mask = graph.buffer(
                "opaque visibility mask",
                BufferDesc {
                    size: self.layout.mask_bytes(),
                },
            )?;
            graph.dispatch(
                "cull packed opaque draws",
                &visibility_program(),
                &[
                    binding(0, camera, VISIBILITY_CAMERA_BYTES as u64),
                    binding(1, aabbs, self.layout.aabb_bytes() as u64),
                    binding(2, mask, self.layout.mask_bytes()),
                ],
                [self.layout.padded_count / VISIBILITY_THREADS, 1, 1],
            )?;
            // The Workspace reserves one slot per material, which bounds groups.
            for group in &self.layout.groups {
                commands.push(graph.encode_indexed_indirect(
                    "encode packed opaque draws",
                    &group.table,
                    vertices,
                    indices,
                    mask,
                    u64::from(group.mask_offset) * VISIBILITY_MASK_BYTES as u64,
                )?);
            }
        }
        Ok(Frame {
            layout: &self.layout,
            vertices,
            indices,
            commands,
        })
    }
}

fn binding(binding: u32, buffer: BufferHandle, size: u64) -> Binding {
    Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size,
        },
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
    pub fn refresh<B: Backend>(self, backend: &B) -> Result<Resident, Error> {
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
    commands: &'a [IndexedIndirectArgs],
}

impl Frame<'_> {
    pub fn visible_direct_draws(
        &self,
        frustum: &Frustum,
        bounds: &[Bounds],
        filter: impl Fn(usize) -> bool,
    ) -> Option<usize> {
        self.commands.is_empty().then(|| {
            self.layout
                .groups
                .iter()
                .filter(|group| filter(group.material))
                .map(|group| group.visible_draws(frustum, bounds).count())
                .sum()
        })
    }

    pub fn draw(
        &self,
        raster: &mut RasterPass<'_, '_>,
        program: &ManagedRasterProgram,
        bindings: &[RasterBindings],
        frustum: &Frustum,
        bounds: &[Bounds],
        filter: impl Fn(usize) -> bool,
    ) -> Result<(), GraphError> {
        for (index, group) in self.layout.groups.iter().enumerate() {
            if !filter(group.material) {
                continue;
            }
            let bindings = Some(bindings[group.material]);
            if let Some(args) = self.commands.get(index) {
                raster.draw_indirect(program, *args, bindings, RasterConstants::default())?;
            } else {
                raster.draw(
                    program,
                    group.visible_draws(frustum, bounds).map(|draw| RasterDraw {
                        bindings,
                        vertices: self.vertices,
                        indices: self.indices,
                        first_index: draw.range.first_index,
                        index_count: draw.range.index_count,
                        instance_count: 1,
                        parameters: RasterConstants::default(),
                    }),
                )?;
            }
        }
        Ok(())
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
    fn indirect_budget_keeps_temporaries_per_in_flight_graph() {
        let scene = Scene::fixture(false);
        let layout = Layout::new(&scene).unwrap().unwrap();
        let mut options = Options::parse(["--fixture".to_owned()]).unwrap();
        assert_eq!(layout.transient_buffers(&options).count(), 0);
        options.submission = SubmissionMode::Indirect;
        options.inflight = 3;
        let sizes = layout.transient_buffers(&options).collect::<Vec<_>>();
        assert_eq!(sizes[0], (layout.mask_bytes(), 3));
        assert_eq!(sizes.len(), 1 + layout.group_count() * 2);
        assert!(sizes.iter().all(|&(_, count)| count == 3));
    }
}

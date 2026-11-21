//! Packed opaque geometry and fixed native indirect command slots.

use super::super::{Error, asset::Scene, camera::Frustum, program, shadow::Bounds};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::{upload_buffer, upload_geometry};
use renderer::{
    passes::visibility::{IndexedGeometry, PreparedVisibility, WorldBounds},
    pbr,
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer_core::graph::GraphPool;
use renderer_core::{
    graph::{
        BufferHandle, Graph, GraphError, ImportBuffer, IndexedIndirectArgs, IndexedIndirectTable,
        IndexedIndirectTemplate, RasterBindings, RasterConstants, RasterDraw, RasterPass,
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
}

impl Group {
    fn visible_draws<'a>(
        &'a self,
        visible: impl Fn(usize) -> bool + 'a,
    ) -> impl Iterator<Item = &'a Draw> {
        self.draws
            .iter()
            .filter(move |draw| visible(draw.primitive))
    }
}

pub(in super::super) struct Layout {
    groups: Vec<Group>,
    visibility: PreparedVisibility,
    pub vertex_bytes: usize,
    pub index_count: u32,
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
        let mut tables = Vec::new();
        let mut vertex_bytes = 0usize;
        let mut index_count = 0u32;
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
            tables.push(IndexedIndirectTable::new(&templates)?);
            groups.push(Group { material, draws });
        }
        let _ = usize::try_from(u64::from(index_count) * size_of::<u32>() as u64)?;
        if groups.is_empty() {
            return Ok(None);
        }
        let visibility = PreparedVisibility::new(&tables)?;
        Ok(Some(Self {
            groups,
            visibility,
            vertex_bytes,
            index_count,
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
        self.visibility.bounds_desc().size as usize
    }

    pub fn visibility(&self) -> &PreparedVisibility {
        &self.visibility
    }

    pub fn template_upload_bytes(&self) -> usize {
        self.visibility.template_transfer_bytes() as usize
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

    fn aabbs(&self, bounds: &[Bounds]) -> Result<Vec<u8>, GraphError> {
        let mut bytes = vec![0; self.aabb_bytes()];
        self.visibility.encode_bounds(&mut bytes, |group, draw| {
            let item = bounds[self.groups[group].draws[draw].primitive];
            WorldBounds {
                min: item.min,
                max: item.max,
                enabled: true,
            }
        })?;
        Ok(bytes)
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
        backend: &mut renderer_core::runtime::Runtime,
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
                &layout.aabbs(bounds)?,
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

    pub fn prepare_frame<'a>(
        &'a self,
        graph: &mut Graph,
        camera: BufferHandle,
        arguments: &'a mut [Option<IndexedIndirectArgs>],
        include_material: impl Fn(usize) -> bool,
    ) -> Result<Frame<'a>, GraphError> {
        let vertices = graph.import_buffer("packed opaque vertices", &self.vertices)?;
        let indices = graph.import_buffer("packed opaque indices", &self.indices)?;
        let commands = if let Some(aabbs) = &self.aabbs {
            let aabbs = graph.import_buffer("packed opaque bounds", aabbs)?;
            let frame =
                self.layout
                    .visibility
                    .add_passes(graph, camera, aabbs, arguments, |group| {
                        include_material(self.layout.groups[group].material)
                            .then_some(IndexedGeometry { vertices, indices })
                    })?;
            Some(frame.arguments)
        } else {
            None
        };
        Ok(Frame {
            layout: &self.layout,
            vertices,
            indices,
            commands,
        })
    }
}

pub(super) struct Frame<'a> {
    layout: &'a Layout,
    vertices: BufferHandle,
    indices: BufferHandle,
    commands: Option<&'a [Option<IndexedIndirectArgs>]>,
}

impl Frame<'_> {
    pub fn is_indirect(&self) -> bool {
        self.commands.is_some()
    }

    pub fn visible_direct_draws(
        &self,
        frustum: &Frustum,
        bounds: &[Bounds],
        include_material: impl Fn(usize) -> bool,
    ) -> Option<usize> {
        (!self.is_indirect()).then(|| {
            self.layout
                .groups
                .iter()
                .filter(|group| include_material(group.material))
                .map(|group| {
                    group
                        .visible_draws(|index| frustum.overlaps(bounds[index]))
                        .count()
                })
                .sum()
        })
    }

    pub fn append_pbr_draws(
        &self,
        out: &mut Vec<pbr::Draw>,
        visible: impl Fn(usize) -> bool,
        include_material: impl Fn(usize) -> bool,
    ) {
        if let Some(commands) = self.commands {
            out.extend(
                self.layout
                    .groups
                    .iter()
                    .zip(commands)
                    .filter_map(|(group, args)| {
                        args.map(|args| pbr::Draw::WorldSpaceIndirect {
                            args,
                            material: group.material,
                        })
                    }),
            );
        } else {
            out.extend(
                self.layout
                    .groups
                    .iter()
                    .filter(|group| include_material(group.material))
                    .flat_map(|group| {
                        group.visible_draws(&visible).map(move |draw| {
                            pbr::Draw::WorldSpaceIndexed(pbr::IndexedDraw {
                                vertices: self.vertices,
                                indices: self.indices,
                                first_index: draw.range.first_index,
                                index_count: draw.range.index_count,
                                material: group.material,
                            })
                        })
                    }),
            );
        }
    }

    pub fn draw(
        &self,
        raster: &mut RasterPass<'_, '_>,
        program: &ManagedRasterProgram,
        bindings: &[RasterBindings],
        frustum: &Frustum,
        bounds: &[Bounds],
        include_material: impl Fn(usize) -> bool,
    ) -> Result<(), GraphError> {
        if let Some(commands) = self.commands {
            for (group, args) in self.layout.groups.iter().zip(commands) {
                if let Some(args) = args {
                    raster.draw_indirect(
                        program,
                        *args,
                        Some(bindings[group.material]),
                        RasterConstants::default(),
                    )?;
                }
            }
        } else {
            let draws = self
                .layout
                .groups
                .iter()
                .filter(|group| include_material(group.material))
                .flat_map(|group| {
                    group
                        .visible_draws(|index| frustum.overlaps(bounds[index]))
                        .map(move |draw| RasterDraw {
                            bindings: Some(bindings[group.material]),
                            vertices: self.vertices,
                            indices: self.indices,
                            first_index: draw.range.first_index,
                            index_count: draw.range.index_count,
                            instance_count: 1,
                            parameters: RasterConstants::default(),
                        })
                });
            raster.draw(program, draws)?;
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
        assert_eq!(first.draws.len(), 2);
        assert_eq!(first.draws[1].range.first_index, 6);
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
        let records = layout.aabbs(&bounds).unwrap();
        let (records, tail) = records.as_chunks::<32>();
        assert_eq!(records.len(), 5 * 64);
        assert!(tail.is_empty());
        assert_eq!(&records[1][0..4], &1.0f32.to_le_bytes());
        assert_eq!(&records[1][12..16], &1u32.to_le_bytes());
        assert_eq!(records[2], [0; 32]);
        let second_group = 64;
        assert_eq!(&records[second_group][0..4], &2.0f32.to_le_bytes());
        assert_eq!(&records[second_group][12..16], &1u32.to_le_bytes());
    }
}

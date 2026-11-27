use super::{CasterDraw, VirtualSun};
use crate::{
    pbr::{Bounds, GraphMaterial, ShadowSettings, Sun, Transform, ViewInput},
    pipeline::Mesh,
};
use sashiko_renderer_core::{
    capacity::Preparation,
    graph::{Graph, GraphCapacityKind, GraphError, RasterDraw},
};

struct Surface {
    mesh: Mesh,
    pose: Transform,
    material: GraphMaterial,
    visible: bool,
    casts: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct CacheTargets<'a> {
    pub atlas: sashiko_renderer_core::graph::ImageHandle,
    pub owners: sashiko_renderer_core::graph::BufferHandle,
    pub reuse: bool,
    pub changed: &'a [Option<Bounds>],
}
pub(crate) struct SceneShadows {
    extent: [u32; 2],
    capacity: usize,
    surfaces: Vec<Surface>,
    receivers: Vec<RasterDraw>,
    casters: Vec<CasterDraw>,
}
impl SceneShadows {
    pub(crate) fn prepare(
        prep: &mut Preparation,
        extent: [u32; 2],
        capacity: usize,
    ) -> Result<Self, GraphError> {
        super::reserve_camera_depth(prep, extent, capacity)?;
        super::reserve_requests(prep)?;
        super::reserve_table(prep)?;
        super::reserve_atlas(prep, capacity)?;
        super::reserve_lighting(prep)?;
        super::reserve_invalidation(prep)?;
        use sashiko_renderer_core::capacity::{GraphRequirements, ResourceClass, WorkingSet};
        prep.add_requirements(
            GraphRequirements {
                resources: 1,
                initial_ranges: 2,
                name_bytes: 80,
                ..Default::default()
            },
            WorkingSet {
                resources: &[ResourceClass::Buffer {
                    desc: sashiko_renderer_core::graph::BufferDesc {
                        size: super::SLOT_BYTES as u64,
                    },
                    count: 1,
                }],
                ..Default::default()
            },
        )?;
        prep.reserve_copy("remember virtual shadow owners")?;
        Ok(Self {
            extent,
            capacity,
            surfaces: Vec::with_capacity(capacity),
            receivers: Vec::with_capacity(capacity),
            casters: Vec::with_capacity(capacity),
        })
    }
    pub(crate) fn clear(&mut self) {
        self.surfaces.clear();
        self.receivers.clear();
        self.casters.clear();
    }
    pub(crate) fn surface(
        &mut self,
        mesh: Mesh,
        pose: Transform,
        material: GraphMaterial,
        visible: bool,
        casts: bool,
    ) -> Result<(), GraphError> {
        if self.surfaces.len() == self.capacity {
            return Err(GraphError::capacity_error(
                GraphCapacityKind::Draws,
                self.capacity,
                self.capacity.checked_add(1),
            ));
        }
        self.surfaces.push(Surface {
            mesh,
            pose,
            material,
            visible,
            casts,
        });
        Ok(())
    }
    pub(crate) fn append(
        &mut self,
        graph: &mut Graph,
        view: ViewInput,
        sun: Sun,
        bounds: Bounds,
        settings: ShadowSettings,
        cache: Option<CacheTargets<'_>>,
    ) -> Result<VirtualSun, GraphError> {
        let camera = super::camera_matrix(graph, view)?;
        for s in &self.surfaces {
            if s.visible
                && let Some(draw) = super::bind_receiver(graph, camera, s.mesh, s.pose, s.material)?
            {
                self.receivers.push(draw);
            }
        }
        let depth = super::draw_camera_depth(graph, self.extent, &self.receivers)?;
        let requests = super::request_pages(graph, depth, view, sun, bounds, settings)?;
        let previous = cache.filter(|c| c.reuse).map(|c| c.owners);
        let pages = super::assign_pages(graph, requests, previous)?;
        let atlas = if let Some(cache) = cache {
            if cache.reuse {
                super::invalidate_casters(graph, &requests, &pages, cache.changed)?;
            }
            graph.copy_buffer(
                "remember virtual shadow owners",
                pages.slots,
                0,
                cache.owners,
                0,
                super::SLOT_BYTES as u64,
            )?;
            cache.atlas
        } else {
            graph.image("directional virtual shadow atlas", super::ATLAS_DESC)?
        };
        for s in &self.surfaces {
            if s.casts
                && let Some(draw) =
                    super::bind_caster(graph, &requests, &pages, s.mesh, s.pose, s.material)?
            {
                self.casters.push(draw);
            }
        }
        super::draw_atlas(graph, atlas, &pages, previous.is_some(), |pass| {
            for draw in &self.casters {
                draw.record(pass)?;
            }
            Ok(())
        })?;
        super::lighting(graph, &requests, &pages, atlas)
    }
}

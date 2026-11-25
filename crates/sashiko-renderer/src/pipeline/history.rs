use super::{classification::Visibility, *};
use crate::mesh::MeshHandle;
use sashiko_renderer_core::graph::GraphCapacityKind;

#[derive(Clone, Copy, PartialEq)]
struct Caster {
    mesh: MeshHandle,
    geometry_revision: u64,
    pose: Option<(crate::mesh::SkinPoseId, u64)>,
    index_range: Option<MeshIndexRange>,
    material: MaterialHandle,
    material_revision: u64,
    transform: Transform,
    tint: [f32; 4],
    coverage: Option<(sashiko_renderer_core::graph::ResourceKey, u64)>,
    external: bool,
}
pub(super) struct History {
    pub(super) maps: pbr::MapHistory<{ pbr::CASCADE_COUNT }>,
    casters: [Vec<Caster>; pbr::CASCADE_COUNT],
    pub(super) revisions: [u64; pbr::CASCADE_COUNT],
    capacity: usize,
}

impl History {
    pub(super) fn new(max_shadow_casters: usize) -> Self {
        Self {
            maps: pbr::MapHistory::default(),
            casters: std::array::from_fn(|_| Vec::with_capacity(max_shadow_casters)),
            revisions: [0; pbr::CASCADE_COUNT],
            capacity: max_shadow_casters,
        }
    }

    pub(super) fn observe(
        &mut self,
        graph: &mut Graph,
        input: SceneInput<'_>,
        results: &[Visibility],
    ) -> Result<(), GraphError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.scene.cascade_history"),
                0,
            )
        });
        // Compare complete ordered caster lists. Hash collisions cannot authorize reuse.
        // Advance the revision before changing any cached records, including on failure.
        let mut counts = [0; pbr::CASCADE_COUNT];
        let mut changed = [false; pbr::CASCADE_COUNT];
        for (object, result) in input.objects.iter().zip(results) {
            if !result.caster {
                continue;
            }
            let state = observe_caster(graph, input, object)?;
            for (i, overlaps) in result.cascades.iter().enumerate() {
                if !overlaps {
                    continue;
                }
                let index = counts[i];
                if index >= self.capacity {
                    return Err(GraphError::capacity_error(
                        GraphCapacityKind::Draws,
                        self.capacity,
                        Some(index + 1),
                    ));
                }
                let list = &mut self.casters[i];
                if state.external || list.get(index) != Some(&state) {
                    if !changed[i] {
                        self.revisions[i] = self.revisions[i]
                            .checked_add(1)
                            .expect("shadow revisions exhausted");
                        changed[i] = true;
                    }
                    if index == list.len() {
                        list.push(state);
                    } else {
                        list[index] = state;
                    }
                }
                counts[i] += 1;
            }
        }
        for (i, len) in counts.into_iter().enumerate() {
            if len != self.casters[i].len() {
                if !changed[i] {
                    self.revisions[i] = self.revisions[i]
                        .checked_add(1)
                        .expect("shadow revisions exhausted");
                }
                self.casters[i].truncate(len);
            }
        }
        Ok(())
    }
}

fn observe_caster(
    graph: &mut Graph,
    input: SceneInput<'_>,
    object: &RenderObject,
) -> Result<Caster, GraphError> {
    let material = input.materials.get(object.material)?;
    let mut coverage = None;
    let external = match material {
        MaterialInstance::Pbr(p) => {
            if let Some(image) = p.coverage_image() {
                // Even an offscreen caster in a cached tile depends on this snapshot.
                graph.import_image("caster coverage", image)?;
                coverage = Some((image.key(), image.revision()));
                graph.image_has_pending_write(image)
            } else {
                false
            }
        }
        MaterialInstance::Unlit(_) => false,
        MaterialInstance::Custom(m) => m
            .definition()
            .shadow()
            .is_some_and(|p| !p.bindings().is_empty()),
    };
    Ok(Caster {
        coverage,
        external,
        mesh: object.mesh,
        geometry_revision: input.meshes.revision(object.mesh)?,
        pose: object
            .pose
            .map(|id| {
                input
                    .meshes
                    .pose_metadata(id, object.mesh)
                    .map(|(_, revision)| (id, revision))
            })
            .transpose()?,
        index_range: object.index_range,
        material: object.material,
        material_revision: input.materials.revision(object.material)?,
        transform: object.transform,
        tint: object.tint,
    })
}

pub(super) struct RectangleHistory {
    pub(super) maps: pbr::RectShadowHistory,
    accepted: Vec<(Caster, Option<Bounds>)>,
    pending: Vec<(Caster, Option<Bounds>)>,
    changed: Vec<Option<Bounds>>,
}

impl RectangleHistory {
    pub(super) fn new(lights: usize, casters: usize) -> Self {
        Self {
            maps: pbr::RectShadowHistory::new(lights),
            accepted: Vec::with_capacity(casters),
            pending: Vec::with_capacity(casters),
            changed: Vec::with_capacity(casters * 2),
        }
    }
    pub(super) fn begin<'a>(
        &'a mut self,
        graph: &mut Graph,
        input: SceneInput<'a>,
        results: &[Visibility],
    ) -> Result<Option<pbr::RectShadowFrame<'a>>, GraphError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.scene.rectangle_history"),
                0,
            )
        });
        self.maps.require_idle()?;
        self.pending.clear();
        self.changed.clear();
        for (object, result) in input.objects.iter().zip(results) {
            if !result.caster {
                continue;
            }
            if self.pending.len() == self.pending.capacity() {
                return Err(GraphError::capacity_error(
                    GraphCapacityKind::Draws,
                    self.pending.capacity(),
                    Some(self.pending.len() + 1),
                ));
            }
            self.pending
                .push((observe_caster(graph, input, object)?, result.bounds));
        }
        for i in 0..self.pending.len().max(self.accepted.len()) {
            let old = self.accepted.get(i);
            let new = self.pending.get(i);
            if let (Some((a, _)), Some((b, _))) = (old, new)
                && a == b
                && !b.external
            {
                continue;
            }
            if let Some((_, b)) = old {
                self.changed.push(*b);
            }
            if let Some((_, b)) = new {
                self.changed.push(*b);
            }
        }
        self.maps.begin(graph, input.lights, |face, previous| {
            let previous = previous.unwrap_or(0);
            if self
                .changed
                .iter()
                .any(|b| b.is_none_or(|b| face.overlaps(b)))
            {
                previous
                    .checked_add(1)
                    .expect("rectangle caster revisions exhausted")
            } else {
                previous
            }
        })
    }
    pub(super) fn accept(
        &mut self,
        receipt: &sashiko_renderer_core::runtime::FrameSubmission,
        updates: &mut pbr::RectShadowUpdates,
    ) -> Result<(), pbr::ShadowHistoryError> {
        self.maps.accept(receipt, updates)?;
        std::mem::swap(&mut self.accepted, &mut self.pending);
        Ok(())
    }
}

use super::*;
use crate::{mesh::MeshHandle, pbr::ShadowHistory};
use renderer::graph::GraphCapacityKind;

#[derive(Clone, Copy, PartialEq)]
struct Caster {
    mesh: MeshHandle,
    geometry_revision: u64,
    material: MaterialHandle,
    material_revision: u64,
    transform: Transform,
    tint: [f32; 4],
}
pub(super) struct History {
    pub(super) maps: ShadowHistory,
    casters: [Vec<Caster>; pbr::CASCADE_COUNT],
    pub(super) revisions: [u64; pbr::CASCADE_COUNT],
    capacity: usize,
}

impl History {
    pub(super) fn new(max_casters_per_cascade: usize) -> Self {
        Self {
            maps: ShadowHistory::default(),
            casters: std::array::from_fn(|_| Vec::with_capacity(max_casters_per_cascade)),
            revisions: [0; pbr::CASCADE_COUNT],
            capacity: max_casters_per_cascade,
        }
    }

    pub(super) fn observe(
        &mut self,
        view: &PreparedView,
        input: SceneInput<'_>,
        draws: &[Draw],
    ) -> Result<(), GraphError> {
        // Compare complete ordered caster lists. Hash collisions cannot authorize reuse.
        // Advance the revision before changing any cached records, including on failure.
        let mut counts = [0; pbr::CASCADE_COUNT];
        let mut changed = [false; pbr::CASCADE_COUNT];
        for (object, draw) in input.objects.iter().zip(draws) {
            if !draw.casts_shadow {
                continue;
            }
            let material = input.materials.get(object.material)?;
            let external = match material {
                MaterialInstance::Pbr(p) if material.alpha_mode() != AlphaMode::Blend => {
                    p.external_shadow_input()
                }
                MaterialInstance::Unlit(_) => false,
                MaterialInstance::Custom(m) => match m.definition().shadow() {
                    Some(program) => !program.bindings().is_empty(),
                    None => continue,
                },
                _ => continue,
            };
            let bounds = draw.world_bounds(material);
            let state = Caster {
                mesh: object.mesh,
                geometry_revision: input.meshes.revision(object.mesh)?,
                material: object.material,
                material_revision: input.materials.revision(object.material)?,
                transform: object.transform,
                tint: object.tint,
            };
            for i in 0..pbr::CASCADE_COUNT {
                if bounds.is_some_and(|b| view.cascade_overlaps(i, b) == Some(false)) {
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
                if external || list.get(index) != Some(&state) {
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

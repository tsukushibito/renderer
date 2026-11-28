use super::ScenePackage;
use crate::ObjectStore;
use glam::Vec3;
use sashiko_renderer::{
    pbr::{Bounds, Transform, ViewCulling},
    pipeline::PipelineResult,
};
use std::collections::HashMap;

impl super::SceneSettings {
    pub fn direct_shadows(&self) -> Option<sashiko_renderer::pbr::ShadowSettings> {
        let defaults = sashiko_renderer::pbr::ShadowSettings::default();
        (self.sun_lux > 0.).then_some(sashiko_renderer::pbr::ShadowSettings {
            resolution: self.shadow_resolution,
            distance: self.shadow_distance,
            virtual_lod_bias: self.virtual_shadow_lod_bias,
            split_lambda: self.shadow_split_lambda.unwrap_or(defaults.split_lambda),
            ..defaults
        })
    }
}

pub(super) fn select(
    scene: &mut ScenePackage,
    store: &ObjectStore,
    cameras: &[sashiko_renderer::pipeline::CameraView],
) -> PipelineResult<Bounds> {
    let index = SceneSpatialIndex::new(scene, store, 8.)?;
    let s = &scene.settings;
    let sun = sashiko_renderer::pbr::Sun::from_lux(s.sun_direction, s.sun_color, s.sun_lux)?;
    let mut selected = vec![false; scene.placements.len()];
    for sample in cameras {
        let view = sample
            .camera
            .culling(index.bounds, sun, s.direct_shadows())?;
        index.visit(&view, |v| selected[v.placement] = true);
    }
    let mut selected = selected.into_iter();
    scene.placements.retain(|_| selected.next().unwrap());
    let mut remap = vec![None; scene.primitives.len()];
    for p in &scene.placements {
        remap[p.primitive] = Some(0);
    }
    for (next, p) in remap.iter_mut().flatten().enumerate() {
        *p = next;
    }
    let mut keep = remap.iter();
    scene.primitives.retain(|_| keep.next().unwrap().is_some());
    if !scene.mesh_lods.is_empty() || !scene.virtual_shadow_meshes.is_empty() {
        let kept: std::collections::HashSet<_> = scene.primitives.iter().map(|p| p.mesh).collect();
        scene.mesh_lods.retain(|lod| kept.contains(&lod.source));
        scene
            .virtual_shadow_meshes
            .retain(|source, _| scene.primitives.iter().any(|p| p.mesh.hash == *source));
    }
    if let Some(physical) = &mut scene.physical {
        let mut keep = remap.iter();
        physical.surfaces.retain(|_| keep.next().unwrap().is_some());
    }
    if let Some(stream) = &mut scene.streaming {
        stream.details.retain_mut(|detail| {
            if let Some(index) = remap[detail.primitive] {
                detail.primitive = index;
                true
            } else {
                false
            }
        });
        if stream.details.is_empty() {
            scene.streaming = None;
        }
    }
    for p in &mut scene.placements {
        p.primitive = remap[p.primitive].unwrap();
    }
    Ok(index.bounds)
}

fn union(a: Bounds, b: Bounds) -> Bounds {
    Bounds::new(
        Vec3::from(a.min()).min(b.min().into()).into(),
        Vec3::from(a.max()).max(b.max().into()).into(),
    )
    .expect("union of finite bounds is finite")
}

struct Entry {
    index: usize,
    bounds: Bounds,
    triangles: u32,
}

struct Cell {
    bounds: Bounds,
    entries: Vec<Entry>,
}

pub struct PlacementVisibility {
    pub placement: usize,
    pub triangles: u32,
    pub camera: bool,
    pub cascades: [bool; 4],
}

/// Immutable world-space bounds for a static package. Each placement belongs
/// to one cell, whose box encloses the entire mesh even across cell borders.
pub struct SceneSpatialIndex {
    cells: Vec<Cell>,
    bounds: Bounds,
    pub unique_meshes: usize,
    pub unique_triangles: u64,
    pub placed_triangles: u64,
}

impl SceneSpatialIndex {
    pub fn new(
        scene: &ScenePackage,
        store: &ObjectStore,
        cell_metres: f32,
    ) -> PipelineResult<Self> {
        if !cell_metres.is_finite() || cell_metres <= 0. {
            return Err("spatial cell width must be positive and finite".into());
        }
        let mut meshes = HashMap::new();
        let mut parts = Vec::with_capacity(scene.primitives.len());
        let mut unique_triangles = 0;
        for (index, part) in scene.primitives.iter().enumerate() {
            let mut mesh = match meshes.entry(part.mesh) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let obj = store.load(&part.mesh)?;
                    let mesh = obj.mesh()?;
                    let triangles = u32::try_from(mesh.indices.len() / 3)?;
                    unique_triangles += u64::from(triangles);
                    *e.insert((mesh.local_bounds()?, triangles))
                }
            };
            if let Some(detail) = scene
                .streaming
                .as_ref()
                .and_then(|s| s.details.iter().find(|d| d.primitive == index))
            {
                if (0..3).any(|axis| {
                    detail.bounds[0][axis] > mesh.0.min()[axis]
                        || detail.bounds[1][axis] < mesh.0.max()[axis]
                }) {
                    return Err("stream bounds exclude fallback geometry".into());
                }
                mesh.0 = Bounds::new(detail.bounds[0], detail.bounds[1])?;
            }
            parts.push(mesh);
        }
        let mut cells: Vec<Cell> = Vec::new();
        let mut by_coord = HashMap::new();
        let mut placed_triangles = 0;
        for (index, p) in scene.placements.iter().enumerate() {
            let &(local, triangles) = parts.get(p.primitive).ok_or("unknown spatial primitive")?;
            let transform = Transform::new(std::array::from_fn(|r| p.transform.map(|col| col[r])))?;
            let bounds = transform
                .bounds(local)
                .ok_or("unrepresentable static placement bounds")?;
            let centre = bounds.center();
            let coord = [centre[0], centre[2]].map(|v| (v / cell_metres).floor() as i64);
            let cell = *by_coord.entry(coord).or_insert_with(|| {
                cells.push(Cell {
                    bounds,
                    entries: Vec::new(),
                });
                cells.len() - 1
            });
            let cell = &mut cells[cell];
            cell.bounds = union(cell.bounds, bounds);
            cell.entries.push(Entry {
                index,
                bounds,
                triangles,
            });
            placed_triangles += u64::from(triangles);
        }
        let bounds = cells
            .iter()
            .map(|c| c.bounds)
            .reduce(union)
            .ok_or("empty spatial index")?;
        Ok(Self {
            cells,
            bounds,
            unique_meshes: meshes.len(),
            unique_triangles,
            placed_triangles,
        })
    }

    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Visit each candidate once, preserving its original placement identity.
    /// Shadow casters can be outside the eye frustum at every depth.
    pub fn visit(&self, view: &ViewCulling, mut emit: impl FnMut(PlacementVisibility)) {
        for cell in &self.cells {
            let visible = view.visible(cell.bounds);
            let clips: [bool; 4] =
                std::array::from_fn(|i| view.cascade_overlaps(i, cell.bounds) == Some(true));
            if !visible && !clips.contains(&true) {
                continue;
            }
            for e in &cell.entries {
                let camera = visible && view.visible(e.bounds);
                let cascades = std::array::from_fn(|i| {
                    clips[i] && view.cascade_overlaps(i, e.bounds) == Some(true)
                });
                if camera || cascades.contains(&true) {
                    emit(PlacementVisibility {
                        placement: e.index,
                        triangles: e.triangles,
                        camera,
                        cascades,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Mat4;

    #[test]
    fn cell_culling_agrees_with_individual_tests_including_offscreen_shadows() {
        let boxes = [0., 1., 5., 6., 30., 31.]
            .map(|x| Bounds::new([x, -0.1, 0.4], [x + 0.5, 0.1, 0.6]).unwrap());
        let bounds = boxes.into_iter().reduce(union).unwrap();
        let cells = boxes
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .map(|(i, chunk)| Cell {
                bounds: union(chunk[0], chunk[1]),
                entries: chunk
                    .iter()
                    .enumerate()
                    .map(|(j, &b)| Entry {
                        index: 2 * i + j,
                        bounds: b,
                        triangles: 1,
                    })
                    .collect(),
            })
            .collect();
        let index = SceneSpatialIndex {
            cells,
            bounds,
            unique_triangles: 1,
            unique_meshes: 1,
            placed_triangles: 6,
        };
        let shadow = [1., 0.5, 0.25, 0.1]
            .map(|scale| Mat4::from_scale(Vec3::new(scale, 1., 1.)).to_cols_array_2d());
        let view = ViewCulling::new(Mat4::IDENTITY.to_cols_array_2d(), Some(&shadow), bounds);
        let mut found = [None; 6];
        index.visit(&view, |p| {
            assert!(found[p.placement].replace((p.camera, p.cascades)).is_none());
        });
        for (i, b) in boxes.into_iter().enumerate() {
            let camera = view.visible(b);
            let cascades = std::array::from_fn(|c| view.cascade_overlaps(c, b).unwrap());
            assert_eq!(
                found[i],
                (camera || cascades.contains(&true)).then_some((camera, cascades))
            );
        }
        assert_eq!(found[2], Some((false, [false, false, false, true])));
        assert!(found[4].is_none());
    }
}

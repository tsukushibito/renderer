//! Shared model geometry, material slots, and per-view level of detail.
use crate::{
    material::MaterialHandle,
    mesh::{MeshHandle, SkinPoseId},
    pbr::{Bounds, Transform},
    pipeline::{CameraView, MeshIndexRange, RenderObject},
};
use sashiko_renderer_core::graph::GraphError;
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Copy)]
pub struct ModelPart {
    pub mesh: MeshHandle,
    pub indices: Option<MeshIndexRange>,
    /// Index in the model's default material table, shared by every LOD.
    pub material_slot: usize,
}

pub struct ModelLod {
    /// Minimum projected width or height in pixels for this level of detail.
    pub min_pixels: f32,
    pub parts: Vec<ModelPart>,
}

/// Immutable model-space bounds, material defaults and geometry. Include the
/// model's intended animation range in its LOD bounds.
pub struct Model {
    bounds: Bounds,
    materials: Vec<MaterialHandle>,
    lods: Vec<ModelLod>,
}

impl Model {
    /// Levels are ordered from finest to coarsest, ending with a zero threshold.
    pub fn new(
        bounds: Bounds,
        materials: Vec<MaterialHandle>,
        lods: Vec<ModelLod>,
    ) -> Result<Arc<Self>, GraphError> {
        if lods.last().is_none_or(|lod| lod.min_pixels != 0.)
            || lods
                .windows(2)
                .any(|pair| pair[0].min_pixels <= pair[1].min_pixels)
        {
            return Err(GraphError::invalid_input(
                "model LOD thresholds must descend to zero",
            ));
        }
        for lod in &lods {
            if !lod.min_pixels.is_finite() || lod.parts.is_empty() {
                return Err(GraphError::invalid_input(
                    "model LOD requires a finite threshold and geometry",
                ));
            }
            if lod
                .parts
                .iter()
                .any(|part| part.material_slot >= materials.len())
            {
                return Err(GraphError::invalid_input("unknown model material slot"));
            }
        }
        Ok(Arc::new(Self {
            bounds,
            materials,
            lods,
        }))
    }

    pub fn instantiate(self: &Arc<Self>) -> ModelInstance {
        ModelInstance {
            model: Arc::clone(self),
            overrides: vec![None; self.materials.len()],
            poses: HashMap::new(),
            transform: Transform::IDENTITY,
        }
    }
}

/// Draw state for one instance. The caller retains its gameplay identity.
pub struct ModelInstance {
    model: Arc<Model>,
    overrides: Vec<Option<MaterialHandle>>,
    poses: HashMap<MeshHandle, SkinPoseId>,
    pub transform: Transform,
}

impl ModelInstance {
    /// Bind a mesh's independent pose across every material slot and LOD
    /// referencing that mesh. Subsequent animation updates only change the palette.
    pub fn set_pose(
        &mut self,
        mesh: MeshHandle,
        pose: Option<SkinPoseId>,
    ) -> Result<(), GraphError> {
        if !self
            .model
            .lods
            .iter()
            .flat_map(|lod| &lod.parts)
            .any(|part| part.mesh == mesh)
        {
            return Err(GraphError::invalid_input(
                "pose mesh is not a part of this model",
            ));
        }
        if let Some(id) = pose {
            self.poses.insert(mesh, id);
        } else {
            self.poses.remove(&mesh);
        }
        Ok(())
    }
    /// Set one instance's slot, or restore the shared default with `None`.
    pub fn set_material(
        &mut self,
        slot: usize,
        material: Option<MaterialHandle>,
    ) -> Result<(), GraphError> {
        *self
            .overrides
            .get_mut(slot)
            .ok_or_else(|| GraphError::invalid_input("unknown model material slot"))? = material;
        Ok(())
    }

    pub(crate) fn max_parts(&self) -> usize {
        self.model
            .lods
            .iter()
            .map(|lod| lod.parts.len())
            .max()
            .unwrap_or(0)
    }

    fn object(&self, part: ModelPart, owns_shadow: bool) -> RenderObject {
        let slot = part.material_slot;
        let material = self.overrides[slot].unwrap_or(self.model.materials[slot]);
        let mut object = RenderObject::new(part.mesh, material).with_transform(self.transform);
        let source = self.model.lods[0].parts[0].mesh;
        object.indirect_shadow_source = if owns_shadow {
            crate::pipeline::IndirectShadowSource::Model {
                mesh: source,
                pose: self.poses.get(&source).copied(),
            }
        } else {
            crate::pipeline::IndirectShadowSource::Disabled
        };
        object.index_range = part.indices;
        object.pose = self.poses.get(&part.mesh).copied();
        object
    }

    /// Enumerate all selected mesh/material pairs for asset preparation, including other LODs.
    pub(crate) fn asset_draws(&self) -> impl Iterator<Item = RenderObject> + '_ {
        self.model
            .lods
            .iter()
            .flat_map(|lod| &lod.parts)
            .enumerate()
            .map(|(i, part)| self.object(*part, i == 0))
    }

    /// Borrow the selected parts for this camera. Independent cameras can extract
    /// different geometry concurrently from the same instance.
    pub fn draws(&self, camera: &CameraView) -> impl ExactSizeIterator<Item = RenderObject> + '_ {
        self.draw_parts(camera).map(|(_, object)| object)
    }

    /// Keep the selected part's slot and mesh range alongside its resolved draw.
    pub fn draw_parts(
        &self,
        camera: &CameraView,
    ) -> impl ExactSizeIterator<Item = (ModelPart, RenderObject)> + '_ {
        let levels = &self.model.lods;
        let level = if levels.len() == 1 {
            &levels[0]
        } else {
            let size = projected_size(self.model.bounds, self.transform, camera);
            &levels[levels.partition_point(|lod| f64::from(lod.min_pixels) > size)]
        };
        level
            .parts
            .iter()
            .enumerate()
            .map(move |(index, &part)| (part, self.object(part, index == 0)))
    }
}

fn projected_size(bounds: Bounds, transform: Transform, camera: &CameraView) -> f64 {
    let world = transform.columns();
    let clip = camera.camera.clip_from_world();
    let mut low = [f64::INFINITY; 2];
    let mut high = [f64::NEG_INFINITY; 2];
    for x in [bounds.min()[0], bounds.max()[0]] {
        for y in [bounds.min()[1], bounds.max()[1]] {
            for z in [bounds.min()[2], bounds.max()[2]] {
                let local = [x, y, z, 1.].map(f64::from);
                let point: [f64; 4] = std::array::from_fn(|r| {
                    (0..4).map(|i| f64::from(world[i][r]) * local[i]).sum()
                });
                let component = |r: usize| {
                    (0..4)
                        .map(|i| f64::from(clip[i][r]) * point[i])
                        .sum::<f64>()
                };
                let w = component(3);
                if w <= 0. {
                    // A box crossing the eye plane has no finite projected bound.
                    return f64::INFINITY;
                }
                for axis in 0..2 {
                    let ndc = component(axis) / w;
                    low[axis] = low[axis].min(ndc);
                    high[axis] = high[axis].max(ndc);
                }
            }
        }
    }
    let pixels = [0, 1].map(|axis| (high[axis] - low[axis]) * 0.5 * f64::from(camera.extent[axis]));
    pixels[0].max(pixels[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        material::{Materials, PbrParams},
        mesh::{MeshCapacity, MeshData, Meshes},
        pbr::{PbrVertex, Projection, ShadowCapsule, ShadowProxy, ViewInput},
        pipeline::{SceneAssets, ViewId},
        unlit_forward::IDENTITY,
    };

    #[test]
    fn material_parts_and_coarse_lods_share_one_proxy_asset_per_instance() {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 2,
            retired_mesh_versions: 1,
        })
        .unwrap();
        let vertices = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]
            .map(|p| PbrVertex::new(p, [0., 0., 1.], [1., 0., 0., 1.], [0.; 2]).unwrap());
        let geometry = || MeshData::pbr(&vertices, &[0, 1, 2]).unwrap();
        let proxy = ShadowProxy {
            capsule: ShadowCapsule::new([0.; 3], [0.; 3], 0.2).unwrap(),
            joint: None,
        };
        let fine = meshes
            .create(geometry().with_shadow_proxies(vec![proxy; 17]).unwrap())
            .unwrap();
        let coarse = meshes.create(geometry()).unwrap();
        let mut materials = Materials::default();
        let material = materials.create_pbr(PbrParams::color([1.; 4])).unwrap();
        let part = |mesh| ModelPart {
            mesh,
            indices: None,
            material_slot: 0,
        };
        let model = Model::new(
            Bounds::new([0.; 3], [1.; 3]).unwrap(),
            vec![material],
            vec![
                ModelLod {
                    min_pixels: 40.,
                    parts: vec![part(fine), part(fine)],
                },
                ModelLod {
                    min_pixels: 0.,
                    parts: vec![part(coarse)],
                },
            ],
        )
        .unwrap();
        let actor = model.instantiate();
        let mut assets = SceneAssets::new(&mut meshes, &mut materials, &[]);
        assets.add_model(&actor);
        assets.add_model(&model.instantiate());
        assert_eq!(assets.shadow_count().unwrap(), 34);
        let camera = ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )
        .unwrap();
        for (pixels, expected_mesh, n) in [(200, fine, 2), (32, coarse, 1)] {
            let view = CameraView {
                camera,
                extent: [pixels; 2],
                id: ViewId(0),
            };
            let mut draws = actor.draws(&view);
            assert_eq!(draws.len(), n);
            let first = draws.next().unwrap();
            assert_eq!(first.mesh, expected_mesh);
            assert_eq!(first.shadow_source(), Some((fine, None)));
            assert!(draws.all(|draw| draw.shadow_source().is_none()));
        }
    }
}

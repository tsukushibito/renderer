//! Shared model geometry, material slots, and per-view level of detail.
use crate::{
    material::MaterialHandle,
    mesh::MeshHandle,
    pbr::{Bounds, Transform},
    pipeline::{CameraView, MeshIndexRange, RenderObject},
};
use renderer_core::graph::GraphError;
use std::sync::Arc;

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

/// Immutable model-space bounds, material defaults and geometry.
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
            transform: Transform::IDENTITY,
        }
    }
}

/// Draw state for one instance. The caller retains its gameplay identity.
pub struct ModelInstance {
    model: Arc<Model>,
    overrides: Vec<Option<MaterialHandle>>,
    pub transform: Transform,
}

impl ModelInstance {
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

    fn object(&self, part: ModelPart) -> RenderObject {
        let slot = part.material_slot;
        let material = self.overrides[slot].unwrap_or(self.model.materials[slot]);
        let mut object = RenderObject::new(part.mesh, material).with_transform(self.transform);
        object.index_range = part.indices;
        object
    }

    /// Enumerate all selected mesh/material pairs for asset preparation, including other LODs.
    pub fn asset_draws(&self) -> impl Iterator<Item = RenderObject> + '_ {
        self.model
            .lods
            .iter()
            .flat_map(|lod| &lod.parts)
            .map(|part| self.object(*part))
    }

    /// Borrow the selected parts for this camera. Independent cameras can extract
    /// different geometry concurrently from the same instance.
    pub fn draws(&self, camera: &CameraView) -> impl ExactSizeIterator<Item = RenderObject> + '_ {
        let levels = &self.model.lods;
        let level = if levels.len() == 1 {
            &levels[0]
        } else {
            let size = projected_size(self.model.bounds, self.transform, camera);
            &levels[levels.partition_point(|lod| f64::from(lod.min_pixels) > size)]
        };
        level.parts.iter().map(|part| self.object(*part))
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

//! Inputs borrowed from the application.
use super::{Bounds, Environment, PointLight, Sun, Transform};
use crate::{
    material::{MaterialHandle, Materials},
    mesh::{MeshHandle, Meshes},
};
use renderer::graph::{Graph, GraphError};

/// Geometry and material selection, independent of any frame Graph or camera.
#[derive(Clone, Copy, Debug)]
pub struct RenderObject {
    pub mesh: MeshHandle,
    pub material: MaterialHandle,
    pub transform: Transform,
    pub sort_center: Option<[f32; 3]>,
    pub tint: [f32; 4],
    pub casts_shadow: bool,
}
impl RenderObject {
    pub fn new(mesh: MeshHandle, material: MaterialHandle) -> Self {
        Self {
            mesh,
            material,
            transform: Transform::IDENTITY,
            sort_center: None,
            casts_shadow: true,
            tint: [1.; 4],
        }
    }
    #[must_use]
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = transform;
        self
    }
}
/// Registered lighting textures shared across cameras and frame Graphs.
pub struct EnvironmentMaps {
    pub irradiance: renderer::graph::ImportImage,
    pub reflection: renderer::graph::ImportImage,
    pub brdf_lut: renderer::graph::ImportImage,
    pub strength: f32,
    pub yaw_radians: f32,
    pub max_prefilter_lod: u32,
}
impl EnvironmentMaps {
    pub fn import(&self, graph: &mut Graph) -> Result<Environment, GraphError> {
        let mut import =
            |texture: &renderer::graph::ImportImage| crate::material::texture_view(graph, texture);
        Ok(Environment {
            irradiance: import(&self.irradiance)?,
            reflection: import(&self.reflection)?,
            brdf_lut: import(&self.brdf_lut)?,
            strength: self.strength,
            yaw_radians: self.yaw_radians,
            max_prefilter_lod: self.max_prefilter_lod,
        })
    }
}

/// Borrow the same object list for any number of cameras.
#[derive(Clone, Copy)]
pub struct SceneInput<'a> {
    pub meshes: &'a Meshes,
    pub materials: &'a Materials,
    pub objects: &'a [RenderObject],
    pub lights: &'a [PointLight],
    /// Directional light for standard shading. Defaults to zero radiance.
    pub sun: Sun,
    /// World bounds for conservative culling and fitting directional shadows.
    /// Optional for unshadowed scenes. Without bounds, all objects remain candidates.
    pub bounds: Option<Bounds>,
    /// Lighting textures imported into the current Graph.
    pub environment: Option<&'a EnvironmentMaps>,
}
impl<'a> SceneInput<'a> {
    pub fn new(meshes: &'a Meshes, materials: &'a Materials, objects: &'a [RenderObject]) -> Self {
        Self {
            meshes,
            materials,
            objects,
            lights: &[],
            sun: Sun::OFF,
            bounds: None,
            environment: None,
        }
    }
    #[must_use]
    pub fn with_sun(mut self, sun: Sun) -> Self {
        self.sun = sun;
        self
    }

    #[must_use]
    pub fn with_bounds(mut self, bounds: Bounds) -> Self {
        self.bounds = Some(bounds);
        self
    }

    #[must_use]
    pub fn with_lights(mut self, lights: &'a [PointLight]) -> Self {
        self.lights = lights;
        self
    }
}

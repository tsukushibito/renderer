//! Inputs borrowed from the application.
use super::{Environment, PointLight, Transform};
use crate::{
    material::{MaterialHandle, Materials},
    mesh::{MeshHandle, Meshes},
};
use renderer::graph::{Graph, GraphError};

/// A nonempty triangle-list range starting at an index in the registered mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshIndexRange {
    first: u32,
    count: u32,
}

impl MeshIndexRange {
    pub fn new(range: std::ops::Range<u32>) -> Result<Self, GraphError> {
        let count = range
            .end
            .checked_sub(range.start)
            .filter(|count| *count > 0 && count.is_multiple_of(3))
            .ok_or_else(|| {
                GraphError::invalid_input("submesh must select one or more complete triangles")
            })?;
        Ok(Self {
            first: range.start,
            count,
        })
    }

    pub fn indices(self) -> std::ops::Range<u32> {
        self.first..self.first + self.count
    }
}

/// Geometry and material selection, independent of any frame Graph or camera.
#[derive(Clone, Copy, Debug)]
pub struct RenderObject {
    pub mesh: MeshHandle,
    pub material: MaterialHandle,
    /// Optional submesh in the shared index buffer.
    pub index_range: Option<MeshIndexRange>,
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
            index_range: None,
            transform: Transform::IDENTITY,
            sort_center: None,
            casts_shadow: true,
            tint: [1.; 4],
        }
    }
    /// Share a vertex/index allocation across several objects or material slots.
    /// Whole-mesh bounds conservatively bound the selected submesh.
    pub fn with_index_range(mut self, range: std::ops::Range<u32>) -> Result<Self, GraphError> {
        self.index_range = Some(MeshIndexRange::new(range)?);
        Ok(self)
    }

    /// Import the drawable mesh revision and select this object's index range.
    pub fn import_mesh(
        &self,
        graph: &mut Graph,
        meshes: &Meshes,
    ) -> Result<super::Mesh, GraphError> {
        let mut mesh = meshes.import(graph, self.mesh)?;
        if let Some(range) = self.index_range {
            if range.indices().end > mesh.index_count {
                return Err(GraphError::invalid_input(
                    "submesh exceeds the drawable index buffer",
                ));
            }
            mesh.first_index = range.first;
            mesh.index_count = range.count;
        }
        Ok(mesh)
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
            environment: None,
        }
    }
    #[must_use]
    pub fn with_lights(mut self, lights: &'a [PointLight]) -> Self {
        self.lights = lights;
        self
    }
}

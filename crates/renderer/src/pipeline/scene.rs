//! Inputs borrowed from the application.
use super::{Bounds, Environment, PointLight, Sun, Transform};
use crate::{
    material::{MaterialHandle, Materials},
    mesh::{MeshHandle, Meshes},
};
use renderer_core::graph::{Graph, GraphError};

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
    /// Optional independent joint pose of this mesh.
    pub pose: Option<crate::mesh::SkinPoseId>,
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
            pose: None,
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
        if self.pose.is_some() {
            return Err(GraphError::invalid_input(
                "import a posed object through its recorded scene input",
            ));
        }
        self.select_range(meshes.import(graph, self.mesh)?)
    }

    fn select_range(&self, mut mesh: super::Mesh) -> Result<super::Mesh, GraphError> {
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
    pub irradiance: renderer_core::graph::ImportImage,
    pub reflection: renderer_core::graph::ImportImage,
    pub brdf_lut: renderer_core::graph::ImportImage,
    pub strength: f32,
    pub yaw_radians: f32,
    pub max_prefilter_lod: u32,
}
impl EnvironmentMaps {
    pub fn import(&self, graph: &mut Graph) -> Result<Environment, GraphError> {
        self.import_diffuse(graph, &self.irradiance)
    }
    pub(super) fn import_diffuse(
        &self,
        graph: &mut Graph,
        irradiance: &renderer_core::graph::ImportImage,
    ) -> Result<Environment, GraphError> {
        let mut import = |texture: &renderer_core::graph::ImportImage| {
            crate::material::texture_view(graph, texture)
        };
        Ok(Environment {
            irradiance: import(irradiance)?,
            local_reflections: None,
            diffuse_lighting: crate::pbr::DiffuseLighting::EnvironmentMap,
            reflection: import(&self.reflection)?,
            brdf_lut: import(&self.brdf_lut)?,
            strength: self.strength,
            yaw_radians: self.yaw_radians,
            max_prefilter_lod: self.max_prefilter_lod,
        })
    }
}

/// Baked scene assets to upload together at a load or level-change boundary.
#[derive(Default, Clone, Copy)]
pub struct ProbeLighting<'a> {
    pub diffuse: Option<&'a crate::pbr::BakedLighting>,
    pub reflections: Option<&'a crate::pbr::ReflectionProbes>,
}

/// Owns the GPU registrations until dropped. Share across cameras on the same renderer.
pub struct ResidentProbeLighting {
    pub(super) diffuse: Option<renderer_core::runtime::ManagedImage>,
    pub(super) reflections: Option<renderer_core::runtime::ManagedImage>,
}

/// A resident diffuse atlas and the two-scenario blend for this view.
#[derive(Clone, Copy)]
pub struct BakedLightingInput<'a> {
    pub atlas: &'a renderer_core::graph::ImportImage,
    pub blend: f32,
    pub view: crate::pbr::BakedLightingView,
}
impl<'a> BakedLightingInput<'a> {
    pub fn new(atlas: &'a renderer_core::graph::ImportImage, blend: f32) -> Self {
        Self {
            atlas,
            blend,
            view: crate::pbr::BakedLightingView::Shaded,
        }
    }
}

/// Resident specular atlas. The standard PBR LUT is used without a global environment.
#[derive(Clone, Copy)]
pub struct ReflectionProbeInput<'a> {
    pub atlas: &'a renderer_core::graph::ImportImage,
    pub blend: f32,
    pub view: crate::pbr::ReflectionProbeView,
}
impl<'a> ReflectionProbeInput<'a> {
    pub fn new(atlas: &'a renderer_core::graph::ImportImage, blend: f32) -> Self {
        Self {
            atlas,
            blend,
            view: Default::default(),
        }
    }
}

/// Borrow the same object list for any number of cameras.
#[derive(Clone, Copy)]
pub struct SceneInput<'a> {
    pub(super) poses: Option<&'a crate::mesh::FramePoses>,
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
    /// Baked diffuse can be used on its own or with the environment's specular maps.
    pub baked_lighting: Option<BakedLightingInput<'a>>,
    pub reflection_probes: Option<ReflectionProbeInput<'a>>,
}
impl<'a> SceneInput<'a> {
    pub fn new(meshes: &'a Meshes, materials: &'a Materials, objects: &'a [RenderObject]) -> Self {
        Self {
            poses: None,
            meshes,
            materials,
            objects,
            lights: &[],
            sun: Sun::OFF,
            bounds: None,
            environment: None,
            baked_lighting: None,
            reflection_probes: None,
        }
    }
    /// Apply the same scenario interpolation to diffuse and specular lighting.
    pub fn with_probe_lighting(mut self, lighting: &'a ResidentProbeLighting, blend: f32) -> Self {
        self.baked_lighting = lighting
            .diffuse
            .as_ref()
            .map(|asset| BakedLightingInput::new(asset.snapshot(), blend));
        self.reflection_probes = lighting
            .reflections
            .as_ref()
            .map(|asset| ReflectionProbeInput::new(asset.snapshot(), blend));
        self
    }
    /// Resolve this frame's posed vertices and the object's selected index range.
    /// All camera and shadow passes reference the same uploaded pose revision.
    pub fn import_mesh(
        &self,
        graph: &mut Graph,
        object: &RenderObject,
    ) -> Result<super::Mesh, GraphError> {
        let Some(id) = object.pose else {
            return object.import_mesh(graph, self.meshes);
        };
        let posed = self
            .poses
            .ok_or_else(|| GraphError::invalid_input("record the scene's poses before drawing"))?
            .stream(id, object.mesh)?;
        object.select_range(
            self.meshes
                .import_geometry(graph, object.mesh, Some(posed))?,
        )
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

use super::{history::History, *};
use crate::{
    mesh::{MeshHandle, Meshes},
    passes::output::SdrImage,
    pbr::ShadowUpdates,
};
use renderer::graph::{GraphCapacityKind, ImageHandle};

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
/// Borrow the same object list for any number of cameras.
#[derive(Clone, Copy)]
pub struct SceneInput<'a> {
    pub meshes: &'a Meshes,
    pub materials: &'a Materials,
    pub objects: &'a [RenderObject],
    pub lights: &'a [PointLight],
    /// Lighting textures imported into the current Graph.
    pub environment: Option<Environment>,
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

/// Camera and per-view history. Use a distinct view for each independent camera.
pub struct View {
    pub camera: ViewInput,
    pub sun: Sun,
    pub bounds: Bounds,
    history: Option<History>,
}
impl View {
    pub fn new(camera: ViewInput, bounds: Bounds) -> Self {
        Self {
            camera,
            bounds,
            sun: Sun::from_normalized([0., 1., 0.], [1.; 3]).expect("unit direction"),
            history: None,
        }
    }

    /// Configure independent shadow reuse at the preparation boundary.
    #[must_use]
    pub fn new_with_shadow_history(
        camera: ViewInput,
        bounds: Bounds,
        max_casters_per_cascade: usize,
    ) -> Self {
        let mut view = Self::new(camera, bounds);
        view.history = Some(History::new(max_casters_per_cascade));
        view
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn clear_history(
        &mut self,
        runtime: &mut renderer::runtime::Runtime,
    ) -> Result<(), pbr::ShadowHistoryError> {
        if let Some(history) = &mut self.history {
            history.maps.clear(runtime)?;
        }
        Ok(())
    }
}

/// Graph-local outputs. Publish shadow history immediately after a successful submission.
#[must_use]
pub struct ViewOutput<T = ()> {
    pub scene: SceneImages,
    pub sdr: T,
    updates: Option<ShadowUpdates>,
}
impl<T> ViewOutput<T> {
    pub fn reused_shadows(&self) -> Option<[bool; pbr::CASCADE_COUNT]> {
        self.updates.as_ref().map(ShadowUpdates::reused)
    }
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn accept(
        &mut self,
        view: &mut View,
        receipt: &renderer::runtime::FrameSubmission,
    ) -> Result<(), pbr::ShadowHistoryError> {
        if let Some(updates) = &mut self.updates {
            view.history
                .as_mut()
                .ok_or(pbr::ShadowHistoryError::ForeignUpdates)?
                .maps
                .accept(receipt, updates)?;
            self.updates = None;
        }
        Ok(())
    }
}

impl RenderPipeline {
    /// Upload the shared default textures once for this Runtime.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn initialize(
        &mut self,
        runtime: &mut renderer::runtime::Runtime,
    ) -> Result<renderer::runtime::FrameId, pbr::DefaultResourceError> {
        if self.resources.is_some() {
            return Err(GraphError::invalid_input("render pipeline already initialized").into());
        }
        let (resources, id) = self.defaults.initialize(runtime)?;
        self.resources = Some(resources);
        Ok(id)
    }

    /// Release this pipeline's default textures at a change boundary.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn clear(
        &mut self,
        runtime: &mut renderer::runtime::Runtime,
    ) -> Result<(), pbr::DefaultResourceError> {
        if let Some(resources) = &mut self.resources {
            resources.clear(runtime)?;
        }
        self.resources = None;
        Ok(())
    }

    /// Standard Forward view without MSAA or shadows.
    pub fn standard(extent: [u32; 2], limits: SceneLimits) -> Result<Self, GraphError> {
        Self::new(Technique::Forward, Config::display(extent), limits, &[])
    }

    /// Include the per-view working set once per simultaneous view in a Graph.
    pub fn reserve_views(
        &self,
        preparation: &mut Preparation,
        count: std::num::NonZeroUsize,
    ) -> Result<(), GraphError> {
        let mut aggregate = Preparation::new(preparation.frames_in_flight());
        aggregate.add_alternatives(&[self.preparation()])?;
        // Runtime registrations for meshes, materials and defaults are shared.
        // Only transient targets and view Graph declarations repeat per camera.
        for _ in 1..count.get() {
            aggregate.add_requirements(
                self.preparation().graph_requirements(),
                self.recipe.preparation().working_set(),
            )?;
        }
        preparation.add_alternatives(&[&aggregate])
    }
    pub fn draw_hdr(
        &mut self,
        graph: &mut Graph,
        view: &mut View,
        input: SceneInput<'_>,
    ) -> Result<ViewOutput, GraphError> {
        self.draw_objects(
            graph,
            view,
            input,
            None::<fn(&mut Graph, pbr::GBuffer) -> Result<(), GraphError>>,
        )
    }

    /// Read geometry depth and material planes at the same Deferred stage.
    pub fn draw_hdr_with_gbuffer(
        &mut self,
        graph: &mut Graph,
        view: &mut View,
        input: SceneInput<'_>,
        consume: impl FnOnce(&mut Graph, pbr::GBuffer) -> Result<(), GraphError>,
    ) -> Result<ViewOutput, GraphError> {
        self.draw_objects(graph, view, input, Some(consume))
    }
    fn draw_objects(
        &mut self,
        graph: &mut Graph,
        view: &mut View,
        input: SceneInput<'_>,
        consume: Option<impl FnOnce(&mut Graph, pbr::GBuffer) -> Result<(), GraphError>>,
    ) -> Result<ViewOutput, GraphError> {
        if input.objects.len() > self.limits.max_draws {
            return Err(GraphError::capacity_error(
                GraphCapacityKind::Draws,
                self.limits.max_draws,
                Some(input.objects.len()),
            ));
        }
        if !input.lights.is_empty() && !self.recipe.technique().supports_point_lights() {
            return Err(GraphError::invalid_input("point lights require Forward+"));
        }
        let defaults = self
            .resources
            .as_ref()
            .ok_or_else(|| {
                GraphError::invalid_input("initialize the render pipeline before drawing a view")
            })?
            .import(graph)?;
        let prepared = self.prepare_view(
            view.camera,
            view.sun,
            view.bounds,
            match input.environment {
                Some(env) => env,
                None => defaults.environment()?,
            },
        )?;
        self.connected.clear();
        for obj in input.objects {
            let instance = input.materials.get(obj.material)?;
            let program = match instance {
                MaterialInstance::Pbr(material) if material.is_resident() => {
                    crate::passes::pbr::opaque_program()
                }
                MaterialInstance::Pbr(_) => {
                    return Err(GraphError::invalid_input(
                        "prepare updated PBR materials before drawing",
                    ));
                }
                MaterialInstance::Unlit(_) => self.unlit.clone(),
                MaterialInstance::Custom(material) => material.definition().forward().clone(),
            };
            let mesh = input.meshes.connect(graph, obj.mesh, &program)?;
            self.connected.push(Draw {
                transform: (obj.transform != Transform::IDENTITY).then_some(obj.transform),
                sort_center: obj.sort_center.unwrap_or_else(|| mesh.bounds.center()),
                casts_shadow: obj.casts_shadow,
                tint: obj.tint,
                ..Draw::new(mesh, obj.material)
            });
        }
        let shadow = if let Some(history) = &mut view.history
            && prepared.shadow_matrices().is_some()
        {
            history.observe(&prepared, input, &self.connected)?;
            Some(
                history
                    .maps
                    .begin_view(graph, &prepared, history.revisions)?,
            )
        } else {
            None
        };
        let connected = std::mem::take(&mut self.connected);
        let mut frame = Frame::new(&prepared, defaults, &connected);
        frame.shadow_history = shadow.as_ref();
        frame.lights = input.lights;
        let scene = match consume {
            Some(callback) => {
                self.draw_connected_hdr_with_gbuffer(graph, input.materials, frame, callback)
            }
            None => self.draw_connected_hdr(graph, input.materials, frame),
        };
        self.connected = connected;
        Ok(ViewOutput {
            scene: scene?,
            sdr: (),
            updates: shadow.map(|s| s.export(graph)).transpose()?,
        })
    }

    /// Draw to the configured SDR output. The final pass writes directly into the supplied
    /// target, or allocates a Graph image when no target is supplied.
    pub fn draw_view(
        &mut self,
        graph: &mut Graph,
        view: &mut View,
        input: SceneInput<'_>,
        exposure: f32,
        target: Option<ImageHandle>,
    ) -> Result<ViewOutput<SdrImage>, GraphError> {
        let (output, exposure) = self.recipe.common().checked_output(exposure)?;
        let result = self.draw_hdr(graph, view, input)?;
        let source = graph.image_view(result.scene.hdr, 0, 0)?;
        let sdr = output.append_to(graph, source, exposure, target)?.sdr;
        Ok(ViewOutput {
            scene: result.scene,
            sdr,
            updates: result.updates,
        })
    }
}

//! Built-in mixed-material implementation of the view contract.
use super::{history::History, *};
use crate::pbr::ShadowUpdates;
use renderer_core::graph::GraphCapacityKind;
use renderer_core::runtime::{FrameId, FrameSubmission, Runtime};
use std::num::NonZeroUsize;
/// Standard pipeline storage associated with one registered camera.
pub struct StandardViewState {
    history: Option<History>,
}
impl StandardViewState {
    pub(super) fn new(shadow_capacity: Option<usize>) -> Self {
        Self {
            history: shadow_capacity.map(History::new),
        }
    }
    pub(super) fn clear_history(
        &mut self,
        runtime: &mut renderer_core::runtime::Runtime,
    ) -> Result<(), pbr::ShadowHistoryError> {
        if let Some(history) = &mut self.history {
            history.maps.clear(runtime)?;
        }
        Ok(())
    }
}

/// Graph-local outputs. Publish shadow history immediately after a successful submission.
#[must_use]
pub struct StandardViewOutput {
    pub scene: SceneImages,
    updates: Option<ShadowUpdates>,
}
impl StandardViewOutput {
    pub fn reused_shadows(&self) -> Option<[bool; pbr::CASCADE_COUNT]> {
        self.updates.as_ref().map(ShadowUpdates::reused)
    }
    pub(super) fn check_submission(
        &self,
        view: &StandardViewState,
        graph_id: u64,
    ) -> Result<(), pbr::ShadowHistoryError> {
        if let Some(update) = &self.updates {
            view.history
                .as_ref()
                .ok_or(pbr::ShadowHistoryError::ForeignUpdates)?
                .maps
                .check_updates(graph_id, update)?;
        }
        Ok(())
    }
    pub(super) fn accept(
        &mut self,
        view: &mut StandardViewState,
        receipt: &renderer_core::runtime::FrameSubmission,
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

impl StandardPipeline {
    pub(super) fn check_object(
        &self,
        input: SceneInput<'_>,
        obj: &RenderObject,
    ) -> Result<(), GraphError> {
        let instance = input.materials.get(obj.material)?;
        if let MaterialInstance::Custom(material) = instance
            && !self.definitions.contains(material.definition())
        {
            return Err(GraphError::invalid_input(
                "custom material definition was not prepared",
            ));
        }
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
        input.meshes.check_layout(obj.mesh, &program)
    }

    /// Upload the shared default textures once for this Runtime.
    pub fn initialize(
        &mut self,
        runtime: &mut renderer_core::runtime::Runtime,
    ) -> Result<renderer_core::runtime::FrameId, pbr::DefaultResourceError> {
        if self.resources.is_some() {
            return Err(GraphError::invalid_input("render pipeline already initialized").into());
        }
        let (resources, id) = self.defaults.initialize(runtime)?;
        self.resources = Some(resources);
        Ok(id)
    }

    /// Release this pipeline's default textures at a change boundary.
    pub fn clear(
        &mut self,
        runtime: &mut renderer_core::runtime::Runtime,
    ) -> Result<(), pbr::DefaultResourceError> {
        if let Some(resources) = &mut self.resources {
            resources.clear(runtime)?;
        }
        self.resources = None;
        Ok(())
    }

    /// Standard Forward view without MSAA or shadows.
    pub fn forward(extent: [u32; 2], limits: SceneLimits) -> Result<Self, GraphError> {
        Self::new(Technique::Forward, StandardConfig::new(extent), limits, &[])
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
    pub(super) fn draw_objects(
        &mut self,
        graph: &mut Graph,
        view: &mut StandardViewState,
        camera: &CameraView,
        input: SceneInput<'_>,
        consume: Option<impl FnOnce(&mut Graph, pbr::GBuffer) -> Result<(), GraphError>>,
    ) -> Result<StandardViewOutput, GraphError> {
        if input.objects.len() > self.limits.max_draws {
            return Err(GraphError::capacity_error(
                GraphCapacityKind::Draws,
                self.limits.max_draws,
                Some(input.objects.len()),
            ));
        }
        let defaults = self
            .resources
            .as_ref()
            .ok_or_else(|| {
                GraphError::invalid_input("initialize the render pipeline before drawing a view")
            })?
            .import(graph)?;
        let mut environment = match (input.environment, input.baked_lighting) {
            (Some(env), Some(baked)) => env.import_diffuse(graph, baked.atlas)?,
            (Some(env), None) => env.import(graph)?,
            (None, Some(baked)) => Environment {
                irradiance: crate::material::texture_view(graph, baked.atlas)?,
                ..defaults.environment()?
            },
            (None, None) => defaults.environment()?,
        };
        if let Some(baked) = input.baked_lighting {
            environment.diffuse_lighting = pbr::DiffuseLighting::BakedLighting {
                blend: baked.blend,
                view: baked.view,
            };
        }
        let prepared = self.prepare_view(camera.camera, input.sun, input.bounds, environment)?;
        self.connected.clear();
        for obj in input.objects {
            let mesh = input.import_mesh(graph, obj)?;
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
        let mut frame = GeometryDrawInput::new(&prepared, defaults, &connected);
        frame.shadow_history = shadow.as_ref();
        frame.lights = input.lights;
        let scene = match consume {
            Some(callback) => {
                self.draw_connected_hdr_with_gbuffer(graph, input.materials, frame, callback)
            }
            None => self.draw_connected_hdr(graph, input.materials, frame),
        };
        self.connected = connected;
        Ok(StandardViewOutput {
            scene: scene?,
            updates: shadow.map(|s| s.export(graph)).transpose()?,
        })
    }
}

impl ViewPipeline for StandardPipeline {
    type State = StandardViewState;
    type Update = StandardViewOutput;

    fn reserve(
        &self,
        preparation: &mut Preparation,
        extent: [u32; 2],
        views: NonZeroUsize,
    ) -> Result<(), GraphError> {
        if self.config.extent != extent {
            return Err(GraphError::invalid_input(
                "standard pipeline extent differs from renderer",
            ));
        }
        self.reserve_views(preparation, views)
    }
    fn initialize(&mut self, runtime: &mut Runtime) -> PipelineResult<Option<FrameId>> {
        Ok(Some(StandardPipeline::initialize(self, runtime)?))
    }
    fn check_object(&self, scene: SceneInput<'_>, object: &RenderObject) -> Result<(), GraphError> {
        StandardPipeline::check_object(self, scene, object)
    }
    fn create_view(&self, _: &CameraView) -> PipelineResult<StandardViewState> {
        Ok(StandardViewState::new(
            self.config
                .shadows
                .map(|_| self.limits.max_casters_per_cascade),
        ))
    }
    fn record(
        &mut self,
        graph: &mut Graph,
        state: &mut StandardViewState,
        camera: &CameraView,
        scene: PreparedScene<'_, Self>,
    ) -> Result<(ViewImages, StandardViewOutput), GraphError> {
        self.record_camera(
            graph,
            state,
            camera,
            scene.input(),
            None::<fn(&mut Graph, crate::pbr::GBuffer) -> Result<(), GraphError>>,
        )
    }
    fn check_submission(
        &self,
        state: &StandardViewState,
        update: &StandardViewOutput,
        graph_id: u64,
    ) -> PipelineResult<()> {
        Ok(update.check_submission(state, graph_id)?)
    }
    fn accept(
        &mut self,
        view: &mut StandardViewState,
        update: &mut StandardViewOutput,
        submission: &FrameSubmission,
    ) -> PipelineResult<()> {
        Ok(update.accept(view, submission)?)
    }
    fn release_view(
        &mut self,
        view: &mut StandardViewState,
        runtime: &mut Runtime,
    ) -> PipelineResult<()> {
        Ok(view.clear_history(runtime)?)
    }
    fn camera_cut(&mut self, view: &mut StandardViewState) -> PipelineResult<()> {
        if let Some(history) = &mut view.history {
            history.maps.invalidate()?;
        }
        Ok(())
    }
    fn release(&mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        Ok(self.clear(runtime)?)
    }
}

impl StandardPipeline {
    pub(super) fn record_camera(
        &mut self,
        graph: &mut Graph,
        state: &mut StandardViewState,
        camera: &CameraView,
        scene: SceneInput<'_>,
        consume: Option<impl FnOnce(&mut Graph, crate::pbr::GBuffer) -> Result<(), GraphError>>,
    ) -> Result<(ViewImages, StandardViewOutput), GraphError> {
        let update = self.draw_objects(graph, state, camera, scene, consume)?;
        Ok((
            ViewImages {
                color: update.scene.hdr,
                depth: update.scene.depth,
            },
            update,
        ))
    }
}

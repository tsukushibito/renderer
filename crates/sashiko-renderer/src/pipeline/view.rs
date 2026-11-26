//! Built-in mixed-material implementation of the view contract.
use super::{history::History, *};
use crate::pbr::ShadowUpdates;
use sashiko_renderer_core::graph::GraphCapacityKind;
use sashiko_renderer_core::runtime::{FrameId, FrameSubmission, Runtime};
use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};
/// Standard pipeline storage associated with one registered camera.
pub struct StandardViewState {
    parallel_next_view: bool,
    history: Option<History>,
    rect_history: Option<super::history::RectangleHistory>,
}
impl StandardViewState {
    pub(super) fn new(shadow_capacity: Option<usize>, rectangles: usize, casters: usize) -> Self {
        Self {
            parallel_next_view: false,
            history: shadow_capacity.map(History::new),
            rect_history: (rectangles > 0)
                .then(|| super::history::RectangleHistory::new(rectangles, casters)),
        }
    }
    pub(super) fn clear_history(
        &mut self,
        runtime: &mut sashiko_renderer_core::runtime::Runtime,
    ) -> Result<(), pbr::ShadowHistoryError> {
        if let Some(history) = &mut self.rect_history {
            history.maps.clear(runtime)?;
        }
        if let Some(history) = &mut self.history {
            history.maps.clear(runtime)?;
        }
        Ok(())
    }
}

/// Host wall time for the StandardPipeline portions of one scene view.
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardViewTimings {
    /// Per-object mesh, pose metadata, index range and lightmap connections.
    pub connect_meshes: Duration,
    /// World bounds, camera and shadow candidates, and transparent depth keys, including job join.
    pub classify: Duration,
    /// Ordered caster history observation and selection of shadow updates for this view.
    pub shadow_history: Duration,
    /// Sorting the visible transparent draw keys.
    pub transparent_sort: Duration,
    /// Camera-visible objects handed to the opaque and transparent lists.
    pub visible_objects: usize,
    /// Objects queued to render each directional shadow cascade in this frame.
    pub cascade_casters: [usize; pbr::CASCADE_COUNT],
}

/// Graph-local outputs. Publish shadow history immediately after a successful submission.
#[must_use]
pub struct StandardViewOutput {
    pub scene: SceneImages,
    timings: Option<StandardViewTimings>,
    updates: Option<ShadowUpdates>,
    rect_updates: Option<pbr::RectShadowUpdates>,
}
impl StandardViewOutput {
    /// CPU subphases measured when scene profiling was enabled for this view.
    pub fn timings(&self) -> Option<StandardViewTimings> {
        self.timings
    }

    pub fn rendered_rectangle_faces(&self) -> usize {
        self.rect_updates.as_ref().map_or(0, |u| u.rendered_faces())
    }
    pub fn reused_rectangle_shadows(&self) -> Option<bool> {
        self.rect_updates.as_ref().map(|u| u.rendered_faces() == 0)
    }
    pub fn reused_shadows(&self) -> Option<[bool; pbr::CASCADE_COUNT]> {
        self.updates.as_ref().map(ShadowUpdates::reused)
    }
    pub(super) fn check_submission(
        &self,
        view: &StandardViewState,
        graph_id: u64,
    ) -> Result<(), pbr::ShadowHistoryError> {
        if let Some(update) = &self.rect_updates {
            view.rect_history
                .as_ref()
                .ok_or(pbr::ShadowHistoryError::ForeignUpdates)?
                .maps
                .check_updates(graph_id, update)?;
        }
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
        receipt: &sashiko_renderer_core::runtime::FrameSubmission,
    ) -> Result<(), pbr::ShadowHistoryError> {
        if let Some(updates) = &mut self.rect_updates {
            view.rect_history
                .as_mut()
                .ok_or(pbr::ShadowHistoryError::ForeignUpdates)?
                .accept(receipt, updates)?;
            self.rect_updates = None;
        }
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
    /// Enable optional per-view host timers for subsequent native frames.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_scene_profiling(&mut self, enabled: bool) {
        self.timings = enabled.then(StandardViewTimings::default);
    }

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
        runtime: &mut sashiko_renderer_core::runtime::Runtime,
    ) -> Result<sashiko_renderer_core::runtime::FrameId, pbr::DefaultResourceError> {
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
        runtime: &mut sashiko_renderer_core::runtime::Runtime,
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
        let imports = sashiko_renderer_core::capacity::GraphRequirements {
            resources: 3,
            initial_ranges: 3,
            name_bytes: 2 * "material texture".len() + "reflection metadata".len(),
            ..Default::default()
        };
        aggregate.add_graph_requirements(imports)?;
        // Runtime registrations for meshes, materials and defaults are shared.
        // Only transient targets and view Graph declarations repeat per camera.
        for _ in 1..count.get() {
            aggregate.add_graph_requirements(imports)?;
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
        allow_jobs: bool,
    ) -> Result<StandardViewOutput, GraphError> {
        if input.objects.len() > self.limits.max_draws {
            return Err(GraphError::capacity_error(
                GraphCapacityKind::Draws,
                self.limits.max_draws,
                Some(input.objects.len()),
            ));
        }
        if input.indirect_shadow_capsules.len() > self.limits.max_indirect_shadow_capsules {
            return Err(GraphError::capacity_error(
                GraphCapacityKind::InputRecords("indirect shadow capsules"),
                self.limits.max_indirect_shadow_capsules,
                Some(input.indirect_shadow_capsules.len()),
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
            (Some(env), Some(baked)) => env.import_diffuse(
                graph,
                baked.atlas,
                defaults.environment()?.reflection_metadata,
            )?,
            (Some(env), None) => env.import(graph, defaults.environment()?.reflection_metadata)?,
            (None, Some(baked)) => Environment {
                irradiance: crate::material::texture_view(graph, baked.atlas)?,
                ..defaults.environment()?
            },
            (None, None) => defaults.environment()?,
        };
        if let Some(local) = input.reflection_probes {
            environment.reflection_metadata =
                graph.import_buffer("reflection metadata", local.metadata)?;
            environment.local_reflections = Some(pbr::LocalReflections {
                atlas: crate::material::texture_view(graph, local.atlas)?,
                blend: local.blend,
                view: local.view,
            });
        }
        if let Some(baked) = input.baked_lighting {
            environment.diffuse_lighting = pbr::DiffuseLighting::BakedLighting {
                blend: baked.blend,
                view: baked.view,
            };
        } else if let Some((baked, blend)) = input.surfaces
            && !baked.pages.is_empty()
        {
            environment.diffuse_lighting = pbr::DiffuseLighting::SurfacePages { blend };
        }
        let mut prepared =
            self.prepare_view(camera.camera, input.sun, input.bounds, environment)?;
        if !input.indirect_shadow_capsules.is_empty()
            && let Some((lighting, blend)) = input.surfaces
            && let Some(volume) = &lighting.shadow_probes
        {
            prepared.set_indirect_shadows(graph, volume, blend, input.indirect_shadow_capsules)?;
        }
        // Adjacent phases share a boundary. Disabled profiling reads no clocks.
        let mut clock = self.timings.map(|_| Instant::now());
        let mut next_phase = || {
            clock.as_mut().map_or(Duration::ZERO, |previous| {
                let now = Instant::now();
                now - std::mem::replace(previous, now)
            })
        };
        {
            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!("renderer.scene.connect_meshes"),
                    0,
                )
            });
            self.connected.clear();
            // Import once per adjacent run on this graph. Ranges, posed bounds
            // and baked placements belong to each instance.
            for run in input.objects.chunk_by(|a, b| a.mesh == b.mesh) {
                let shared = input.meshes.import(graph, run[0].mesh)?;
                for obj in run {
                    // Classify using the current deformed bounds before requesting vertices.
                    let mut mesh = obj.select_range(shared)?;
                    if let Some(id) = obj.pose {
                        mesh.bounds = input.meshes.pose_metadata(id, obj.mesh)?.0;
                    }
                    let lightmap = match (obj.placement(), input.surfaces) {
                        (Some(id), Some((baked, _))) if !baked.pages.is_empty() => {
                            match baked.placements.get(&id).copied() {
                                Some(address) => {
                                    let uv = input.meshes.import_lightmap_uvs(graph, obj.mesh)?;
                                    let image = crate::material::texture_view(
                                        graph,
                                        baked.pages[address.page].snapshot(),
                                    )?;
                                    Some(pbr::DrawLightmap::new(uv, image, address))
                                }
                                None if baked.complete => {
                                    return Err(GraphError::invalid_input(
                                        "unknown baked placement",
                                    ));
                                }
                                None => None,
                            }
                        }
                        _ => None,
                    };
                    self.connected.push(Draw {
                        lightmap,
                        transform: (obj.transform != Transform::IDENTITY).then_some(obj.transform),
                        sort_center: obj.sort_center.unwrap_or_else(|| mesh.bounds.center()),
                        camera_visible: obj.camera_visible,
                        casts_shadow: obj.casts_shadow,
                        tint: obj.tint,
                        ..Draw::new(mesh, obj.material)
                    });
                }
            }
        }
        if let Some(timings) = &mut self.timings {
            timings.connect_meshes = next_phase();
        }
        self.classification.run(
            &self.connected,
            input.materials,
            &prepared,
            self.config.shadows.is_some(),
            input.lights,
            allow_jobs,
        )?;
        if let Some(timings) = &mut self.timings {
            timings.classify = next_phase();
        }
        let results = &self.classification.results[..self.connected.len()];
        let shadow = if let Some(history) = &mut view.history
            && prepared.shadow_matrices().is_some()
        {
            history.observe(graph, input, results)?;
            Some(
                history
                    .maps
                    .begin_view(graph, &prepared, history.revisions)?,
            )
        } else {
            None
        };
        let rect_frame = if input.lights.iter().any(|l| l.shadow_emitter().is_some()) {
            let history = view.rect_history.as_mut().ok_or_else(|| {
                GraphError::capacity_error(pbr::SceneLimits::SHADOWED_RECT_LIGHTS, 0, Some(1))
            })?;
            history.begin(graph, input, results)?
        } else {
            None
        };
        if let Some(timings) = &mut self.timings {
            timings.shadow_history = next_phase();
        }
        let connected = std::mem::take(&mut self.connected);
        let mut frame = GeometryDrawInput::new(&prepared, defaults, &connected);
        frame.rect_target = rect_frame.as_ref().map(|f| f.targets);
        frame.shadow_history = shadow.as_ref();
        frame.lights = input.lights;
        frame.scene = Some(input);
        let scene =
            self.declare_classified(graph, input.materials, frame, |recipe, graph, frame| {
                match consume {
                    Some(callback) => recipe.draw_hdr_with_gbuffer(graph, frame, callback),
                    None => recipe.draw_hdr(graph, frame),
                }
            });
        self.connected = connected;
        if let Some(timings) = &mut self.timings {
            timings.visible_objects = self.opaque.len()
                + self.transparent.len()
                + self.forward.opaque.len()
                + self.forward.blend.len();
            timings.cascade_casters =
                std::array::from_fn(|i| self.casters[i].len() + self.forward.shadow[i].len());
        }
        Ok(StandardViewOutput {
            scene: scene?,
            timings: self.timings,
            rect_updates: rect_frame.map(|f| f.export(graph)).transpose()?,
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
            self.config.shadows.map(|_| self.limits.max_shadow_casters),
            self.limits.max_shadowed_rect_lights,
            self.limits.max_shadow_casters,
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
        view.parallel_next_view = false;
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
        // Only scheduling uses the previous output of this camera. Each frame
        // still classifies its actual objects and evaluates current shadow history.
        let parallel = self.auto_jobs.is_none()
            || (scene.objects.len() >= StandardJobPolicy::MIN_OBJECTS && state.parallel_next_view);
        let record =
            |graph: &mut Graph| self.draw_objects(graph, state, camera, scene, consume, parallel);
        let update = if parallel {
            record(graph)
        } else {
            graph.with_serial_raster(record)
        }?;
        if let Some(policy) = self.auto_jobs {
            let opaque = self.transparent.is_empty() && self.forward.blend.is_empty();
            state.parallel_next_view = opaque
                && update.rect_updates.is_none()
                && if let Some(reused) = update.reused_shadows() {
                    policy.updating_shadows && reused.contains(&false)
                } else {
                    self.opaque.is_empty()
                };
        }
        Ok((
            ViewImages {
                color: update.scene.hdr,
                depth: update.scene.depth,
            },
            update,
        ))
    }
}

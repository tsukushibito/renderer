//! Draw mixed materials into one scene-linear HDR view.
pub use crate::pbr::{
    Bounds, CameraStorage, Config, Environment, PointLight, PreparedView, Projection, SceneImages,
    SceneLimits, ShadowSettings, Sun, Transform, ViewInput,
};
use crate::{
    material::{MaterialDefinition, MaterialHandle, MaterialInstance, Materials},
    pbr::{self, AlphaMode, DefaultViews},
};
use renderer::{
    capacity::{GraphRequirements, Preparation, WorkingSet},
    graph::{
        Graph, GraphError, RasterBindings, RasterDraw, RasterParameters, RasterPass, SampleCount,
    },
    program::ManagedRasterProgram,
};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

mod geometry;
mod history;
pub use geometry::{GeometryPipeline, Technique};
mod input;
mod view;
pub use input::{Draw, Frame, Mesh};
pub use view::{StandardViewOutput, StandardViewState};
mod scene;
pub use scene::{EnvironmentMaps, MeshIndexRange, RenderObject, SceneInput};
mod scene_pairs;
pub use scene_pairs::{FrameScene, PipelineId};
mod contract;
pub use contract::*;

struct ForwardCommand {
    program: ManagedRasterProgram,
    draw: RasterDraw,
}
impl ForwardCommand {
    fn record(&self, pass: &mut RasterPass<'_, '_>) -> Result<(), GraphError> {
        pass.draw(&self.program, [self.draw])
    }
}

/// Prepared graph operands, borrowed only during recipe declaration.
pub(crate) struct ForwardDraws {
    opaque: Vec<ForwardCommand>,
    blend: Vec<(usize, ForwardCommand)>,
    pbr_blends: usize,
    shadow: [Vec<ForwardCommand>; 4],
}
impl ForwardDraws {
    pub(crate) fn has_blend(&self) -> bool {
        !self.blend.is_empty()
    }
    pub(crate) fn has_opaque(&self) -> bool {
        !self.opaque.is_empty()
    }
    pub(crate) fn draw_opaque(&self, pass: &mut RasterPass<'_, '_>) -> Result<(), GraphError> {
        for command in &self.opaque {
            command.record(pass)?
        }
        Ok(())
    }
    pub(crate) fn draw_shadow(
        &self,
        pass: &mut RasterPass<'_, '_>,
        cascade: usize,
    ) -> Result<(), GraphError> {
        for command in &self.shadow[cascade] {
            command.record(pass)?
        }
        Ok(())
    }
    pub(crate) fn draw_color(
        &self,
        pass: &mut RasterPass<'_, '_>,
        mut pbr: impl FnMut(&mut RasterPass<'_, '_>, Range<usize>) -> Result<(), GraphError>,
    ) -> Result<(), GraphError> {
        let mut start = 0;
        for (before, cmd) in &self.blend {
            if start < *before {
                pbr(pass, start..*before)?;
            }
            cmd.record(pass)?;
            start = *before;
        }
        if start < self.pbr_blends {
            pbr(pass, start..self.pbr_blends)?
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct Resolved {
    pbr: Option<usize>,
    color: Option<RasterBindings>,
    shadow: Option<RasterBindings>,
}
/// Settings for the standard scene-linear HDR view.
#[derive(Clone, Copy, Debug)]
pub struct StandardConfig {
    pub extent: [u32; 2],
    pub clear_color: [f32; 4],
    pub shadows: Option<ShadowSettings>,
    pub samples: SampleCount,
}

impl StandardConfig {
    /// Single-sample view with an opaque black background.
    pub fn new(extent: [u32; 2]) -> Self {
        Self {
            extent,
            clear_color: [0., 0., 0., 1.],
            shadows: None,
            samples: SampleCount::X1,
        }
    }
}

/// Prepared drawing techniques and bounded per-view scratch.
pub struct StandardPipeline {
    resources: Option<pbr::DefaultResources>,
    recipe: GeometryPipeline,
    preparation: Preparation,
    defaults: pbr::PreparedDefaults,
    config: pbr::Config,
    limits: pbr::SceneLimits,
    definitions: HashSet<MaterialDefinition>,
    unlit: ManagedRasterProgram,
    cache: HashMap<MaterialHandle, Resolved>,
    materials: Vec<pbr::GraphMaterial>,
    opaque: Vec<pbr::Draw>,
    transparent: Vec<pbr::Draw>,
    casters: [Vec<pbr::Draw>; pbr::CASCADE_COUNT],
    transforms: Vec<Transform>,
    connected: Vec<Draw>,
    order: Vec<(f64, usize, [bool; pbr::CASCADE_COUNT])>,
    forward: ForwardDraws,
}
impl StandardPipeline {
    /// Prepare once, including all custom definitions that may be used by this scene.
    pub fn new(
        technique: pbr::Technique,
        config: StandardConfig,
        limits: pbr::SceneLimits,
        definitions: &[MaterialDefinition],
    ) -> Result<Self, GraphError> {
        if !definitions.is_empty() && config.samples != SampleCount::X1 {
            return Err(GraphError::invalid_input(
                "custom materials support Forward 1x indexed draws only",
            ));
        }
        let config = pbr::Config {
            extent: config.extent,
            clear_color: config.clear_color,
            shadows: config.shadows,
            samples: config.samples,
            camera_storage: CameraStorage::Transient,
            output: None,
        };
        let recipe = GeometryPipeline::new(technique, config, limits)?;
        let mut preparation = Preparation::new(limits.frames_in_flight);
        preparation.add_alternatives(&[recipe.preparation()])?;
        let defaults = pbr::Defaults::for_recipe(config).prepare(&mut preparation)?;
        let unlit = ManagedRasterProgram::mesh_hdr();
        let mut raster = vec![renderer::program::RasterPipelineRequest {
            program: unlit.clone(),
            sample_count: config.samples,
        }];
        if config.shadows.is_some() {
            raster.push(ManagedRasterProgram::depth_only().into());
        }
        for def in definitions {
            raster.push(def.forward().clone().into());
            if let Some(shadow) = def.shadow() {
                raster.push(shadow.clone().into())
            }
        }
        let max = limits.max_draws;
        let mats = limits.max_materials;
        let shadow_max = if config.shadows.is_some() {
            limits.max_casters_per_cascade
        } else {
            0
        };
        let overflow = || GraphError::invalid_input("mixed scene capacity overflow");
        let draws = shadow_max
            .checked_mul(4)
            .and_then(|s| max.checked_add(s))
            .ok_or_else(overflow)?;
        let count = |n: usize, k| n.checked_mul(k).ok_or_else(overflow);
        let bindings = definitions
            .iter()
            .map(|d| {
                d.forward()
                    .bindings()
                    .len()
                    .max(d.shadow().map_or(0, |p| p.bindings().len()))
            })
            .max()
            .unwrap_or(0);
        let resources = count(mats, 1 + (2 * bindings).max(4))?;
        preparation.reserve_buffer(pbr::MaterialUniform::DESC, mats)?;
        preparation.add_requirements(
            GraphRequirements {
                resources: resources.checked_add(count(max, 2)?).ok_or_else(overflow)?,
                passes: mats,
                accesses: count(draws, bindings)?
                    .checked_add(mats)
                    .ok_or_else(overflow)?,
                bindings: count(mats, 2 * bindings)?,
                raster_constant_words: count(draws, 32)?,
                upload_bytes: count(mats, crate::passes::pbr::MATERIAL_BYTES)?,
                name_bytes: count(resources, 40)?
                    .checked_add(count(max, 25)?)
                    .ok_or_else(overflow)?,
                ..Default::default()
            },
            WorkingSet {
                raster: &raster,
                ..Default::default()
            },
        )?;
        Ok(Self {
            resources: None,
            recipe,
            preparation,
            defaults,
            config,
            limits,
            definitions: definitions.iter().cloned().collect(),
            unlit,
            cache: HashMap::with_capacity(mats),
            materials: Vec::with_capacity(mats),
            opaque: Vec::with_capacity(max),
            transparent: Vec::with_capacity(max),
            casters: std::array::from_fn(|_| Vec::with_capacity(shadow_max)),
            transforms: Vec::with_capacity(limits.max_transforms),
            connected: Vec::with_capacity(max),
            order: Vec::with_capacity(max),
            forward: ForwardDraws {
                opaque: Vec::with_capacity(max),
                blend: Vec::with_capacity(max),
                pbr_blends: 0,
                shadow: std::array::from_fn(|_| Vec::with_capacity(shadow_max)),
            },
        })
    }
    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }
    pub fn defaults(&self) -> &pbr::PreparedDefaults {
        &self.defaults
    }
    pub fn prepare_view(
        &self,
        input: ViewInput,
        sun: Sun,
        bounds: impl Into<Option<Bounds>>,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        self.recipe.prepare_view(input, sun, bounds, environment)
    }
    pub fn draw_connected_hdr(
        &mut self,
        graph: &mut Graph,
        materials: &Materials,
        frame: Frame<'_>,
    ) -> Result<SceneImages, GraphError> {
        self.declare(graph, materials, frame, |recipe, graph, frame, lights| {
            recipe.draw_hdr(graph, frame, lights)
        })
    }

    /// Read coherent Deferred material planes and depth before Forward opaque draws.
    pub fn draw_connected_hdr_with_gbuffer(
        &mut self,
        graph: &mut Graph,
        materials: &Materials,
        frame: Frame<'_>,
        consume: impl FnOnce(&mut Graph, pbr::GBuffer) -> Result<(), GraphError>,
    ) -> Result<SceneImages, GraphError> {
        if self.recipe.technique() != Technique::Deferred || !frame.lights.is_empty() {
            return Err(GraphError::invalid_input(
                "G-buffer access requires Deferred with no point lights",
            ));
        }
        self.declare(graph, materials, frame, |recipe, graph, frame, _| {
            let GeometryPipeline::Deferred(recipe) = recipe else {
                unreachable!()
            };
            recipe.add_scene_with_gbuffer(graph, frame, consume)
        })
    }

    fn declare(
        &mut self,
        graph: &mut Graph,
        materials: &Materials,
        frame: Frame<'_>,
        append: impl FnOnce(
            &mut GeometryPipeline,
            &mut Graph,
            pbr::Frame<'_>,
            &[PointLight],
        ) -> Result<SceneImages, GraphError>,
    ) -> Result<SceneImages, GraphError> {
        let Frame {
            view,
            defaults,
            draws,
            lights,
            camera,
            shadow_history,
        } = frame;
        let render_shadow: [bool; pbr::CASCADE_COUNT] = std::array::from_fn(|i| {
            self.config.shadows.is_some()
                && shadow_history.is_none_or(|history| !history.is_reused(i))
        });
        if draws.len() > self.limits.max_draws {
            return Err(GraphError::capacity_error(
                renderer::graph::GraphCapacityKind::Draws,
                self.limits.max_draws,
                Some(draws.len()),
            ));
        }
        self.cache.clear();
        self.materials.clear();
        self.opaque.clear();
        self.transparent.clear();
        for casters in &mut self.casters {
            casters.clear();
        }
        self.transforms.clear();
        self.order.clear();
        self.forward.opaque.clear();
        self.forward.blend.clear();
        for s in &mut self.forward.shadow {
            s.clear()
        }
        for (i, draw) in draws.iter().enumerate() {
            let instance = materials.get(draw.material)?;
            let blend = instance.alpha_mode() == AlphaMode::Blend;
            let casts_shadow = draw.casts_shadow
                && match instance {
                    MaterialInstance::Pbr(_) => !blend,
                    MaterialInstance::Custom(m) => m.definition().shadow().is_some(),
                    MaterialInstance::Unlit(_) => true,
                };
            let bounds = draw.world_bounds(instance);
            let visible = bounds.is_none_or(|b| view.visible(b));
            let cascades = std::array::from_fn(|i| {
                render_shadow[i]
                    && casts_shadow
                    && bounds.is_none_or(|b| view.cascade_overlaps(i, b) != Some(false))
            });
            if !visible && !cascades.contains(&true) {
                continue;
            }
            if blend && visible {
                let depth = draw.view_depth(view)?;
                self.order.push((depth, i, cascades));
            } else {
                self.record(graph, view, defaults, draw, instance, visible, cascades)?;
            }
        }
        self.order
            .sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        for index in 0..self.order.len() {
            let draw = &draws[self.order[index].1];
            self.record(
                graph,
                view,
                defaults,
                draw,
                materials.get(draw.material)?,
                true,
                self.order[index].2,
            )?;
        }
        self.forward.pbr_blends = self.transparent.len();
        let mut frame = pbr::Frame::new(
            view,
            &self.materials,
            &self.opaque,
            &self.transparent,
            &[],
            defaults,
        )?;
        if self.config.shadows.is_some() {
            frame.shadows = pbr::FrameShadows::Cascades(std::array::from_fn(|i| {
                let casters = self.casters[i].as_slice();
                shadow_history.map_or(
                    pbr::ShadowSource::Render {
                        target: None,
                        casters,
                    },
                    |h| h.source(i, casters),
                )
            }));
        }
        frame.forward_draws = Some(&self.forward);
        frame.transforms = &self.transforms;
        frame.camera_target = camera;
        append(&mut self.recipe, graph, frame, lights)
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "record one classified draw and its view visibility"
    )]
    fn record(
        &mut self,
        graph: &mut Graph,
        view: &PreparedView,
        defaults: DefaultViews,
        draw: &Draw,
        instance: &MaterialInstance,
        visible: bool,
        cascades: [bool; pbr::CASCADE_COUNT],
    ) -> Result<(), GraphError> {
        let expected = match instance {
            MaterialInstance::Pbr(_) => crate::passes::pbr::VERTEX_BYTES as u32,
            MaterialInstance::Unlit(_) => self.unlit.vertex_stride(),
            MaterialInstance::Custom(m) => m.definition().forward().vertex_stride(),
        };
        if draw.mesh.vertex_stride != expected {
            return Err(GraphError::invalid_input(
                "mesh vertex stride does not match material program",
            ));
        }
        let resolved = self.resolve(graph, defaults, draw.material, instance)?;
        let blend = instance.alpha_mode() == AlphaMode::Blend;
        let mesh = draw.mesh;
        if let Some(material) = resolved.pbr {
            let geometry = pbr::IndexedDraw {
                vertices: mesh.vertices,
                indices: mesh.indices,
                first_index: mesh.first_index,
                index_count: mesh.index_count,
                material,
            };
            let command = if let Some(transform) = draw.transform {
                if self.transforms.len() >= self.limits.max_transforms {
                    return Err(GraphError::capacity_error(
                        renderer::graph::GraphCapacityKind::Draws,
                        self.limits.max_transforms,
                        Some(self.transforms.len() + 1),
                    ));
                }
                let index = self.transforms.len() as u32;
                self.transforms.push(transform);
                pbr::Draw::model(geometry, index)
            } else {
                pbr::Draw::WorldSpaceIndexed(geometry)
            };
            if blend {
                if visible {
                    self.transparent.push(command);
                }
            } else {
                if visible {
                    self.opaque.push(command);
                }
                for (i, enabled) in cascades.into_iter().enumerate() {
                    if !enabled {
                        continue;
                    }
                    self.check_caster_capacity(i)?;
                    self.casters[i].push(command);
                }
            }
            return Ok(());
        }
        let model = draw.transform.unwrap_or(Transform::IDENTITY).columns();
        let clip = product(*view.matrix(), model);
        let command = |program: &ManagedRasterProgram, bindings, parameters| ForwardCommand {
            program: program.clone(),
            draw: RasterDraw {
                vertices: mesh.vertices,
                indices: mesh.indices,
                first_index: mesh.first_index,
                index_count: mesh.index_count,
                instance_count: 1,
                bindings,
                parameters,
            },
        };
        let unlit_shadow = ManagedRasterProgram::depth_only();
        let unlit = self.unlit.clone();
        let (program, shadow) = match instance {
            MaterialInstance::Unlit(_) => (&unlit, Some(&unlit_shadow)),
            MaterialInstance::Custom(p) => (p.definition().forward(), p.definition().shadow()),
            MaterialInstance::Pbr(_) => unreachable!(),
        };
        let constants = |matrix| match instance {
            MaterialInstance::Unlit(p) => {
                RasterParameters::new(matrix, std::array::from_fn(|k| p.color[k] * draw.tint[k]))
                    .map(Into::into)
            }
            MaterialInstance::Custom(p) => p.constants(matrix),
            MaterialInstance::Pbr(_) => unreachable!(),
        };
        if draw.casts_shadow
            && let (Some(program), Some(matrices)) = (shadow, view.shadow_matrices())
        {
            for (i, matrix) in matrices.iter().enumerate() {
                if !cascades[i] {
                    continue;
                }
                self.check_caster_capacity(i)?;
                self.forward.shadow[i].push(command(
                    program,
                    resolved.shadow,
                    constants(product(*matrix, model))?,
                ))
            }
        }
        if !visible {
            return Ok(());
        }
        let cmd = command(program, resolved.color, constants(clip)?);
        if blend {
            self.forward.blend.push((self.transparent.len(), cmd))
        } else {
            self.forward.opaque.push(cmd)
        }

        Ok(())
    }
    fn check_caster_capacity(&self, cascade: usize) -> Result<(), GraphError> {
        let required = self.casters[cascade].len() + self.forward.shadow[cascade].len() + 1;
        if required > self.limits.max_casters_per_cascade {
            return Err(GraphError::capacity_error(
                renderer::graph::GraphCapacityKind::Draws,
                self.limits.max_casters_per_cascade,
                Some(required),
            ));
        }
        Ok(())
    }

    fn resolve(
        &mut self,
        graph: &mut Graph,
        defaults: DefaultViews,
        handle: MaterialHandle,
        instance: &MaterialInstance,
    ) -> Result<Resolved, GraphError> {
        if self.cache.len() == self.limits.max_materials && !self.cache.contains_key(&handle) {
            return Err(GraphError::capacity_error(
                renderer::graph::GraphCapacityKind::Bindings,
                self.limits.max_materials,
                Some(self.cache.len() + 1),
            ));
        }
        let resolved = match self.cache.entry(handle) {
            std::collections::hash_map::Entry::Occupied(slot) => slot.into_mut(),
            std::collections::hash_map::Entry::Vacant(slot) => {
                let mut result = Resolved {
                    pbr: None,
                    color: None,
                    shadow: None,
                };
                match instance {
                    MaterialInstance::Pbr(p) => {
                        result.pbr = Some(self.materials.len());
                        self.materials.push(p.resolve(
                            graph,
                            "material factors",
                            Some(defaults),
                        )?);
                    }
                    MaterialInstance::Custom(p) => {
                        let def = p.definition();
                        if !self.definitions.contains(def) {
                            return Err(GraphError::invalid_input(
                                "material definition was not prepared for this scene",
                            ));
                        }
                        result.color = p.bindings(graph, def.forward())?;
                        if self.config.shadows.is_some()
                            && let Some(s) = def.shadow()
                        {
                            result.shadow = p.bindings(graph, s)?
                        }
                    }
                    MaterialInstance::Unlit(_) => {}
                }
                slot.insert(result)
            }
        };
        Ok(*resolved)
    }
}
fn product(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    std::array::from_fn(|col| {
        std::array::from_fn(|row| (0..4).map(|k| a[k][row] * b[col][k]).sum())
    })
}

mod driver;
pub use driver::*;

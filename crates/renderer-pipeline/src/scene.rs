//! Draw mixed materials into one scene-linear HDR view.
pub use crate::pbr::{
    Bounds, CameraStorage, Config, Environment, PointLight, PreparedView, Projection, SceneImages,
    SceneLimits, ShadowSettings, Sun, Technique, Transform, ViewInput,
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

mod input;
pub use input::{Draw, Frame, Mesh};

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
pub struct Scene {
    recipe: pbr::Prepared,
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
    casters: Vec<pbr::Draw>,
    transforms: Vec<Transform>,
    order: Vec<(f64, usize)>,
    forward: ForwardDraws,
}
impl Scene {
    /// Prepare once, including all custom definitions that may be used by this scene.
    pub fn new(
        technique: pbr::Technique,
        config: pbr::Config,
        limits: pbr::SceneLimits,
        definitions: &[MaterialDefinition],
    ) -> Result<Self, GraphError> {
        if !definitions.is_empty() && config.samples != SampleCount::X1 {
            return Err(GraphError::invalid_input(
                "custom materials support Forward 1x indexed draws only",
            ));
        }
        let recipe = pbr::Prepared::new(technique, config, limits)?;
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
            .map(|d| d.forward().bindings().len())
            .max()
            .unwrap_or(0);
        let resources = count(mats, 1 + bindings.max(4))?;
        preparation.reserve_buffer(pbr::MaterialUniform::DESC, mats)?;
        preparation.add_requirements(
            GraphRequirements {
                resources,
                passes: mats,
                accesses: count(draws, bindings)?
                    .checked_add(mats)
                    .ok_or_else(overflow)?,
                bindings: count(mats, 2 * bindings)?,
                raster_constant_words: count(draws, 32)?,
                upload_bytes: count(mats, crate::passes::pbr::MATERIAL_BYTES)?,
                name_bytes: count(resources, 40)?,
                ..Default::default()
            },
            WorkingSet {
                raster: &raster,
                ..Default::default()
            },
        )?;
        Ok(Self {
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
            casters: Vec::with_capacity(shadow_max),
            transforms: Vec::with_capacity(limits.max_transforms),
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
        bounds: Bounds,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        self.recipe.prepare_view(input, sun, bounds, environment)
    }
    pub fn add_output(
        &self,
        graph: &mut Graph,
        hdr: renderer::graph::ImageHandle,
        exposure: f32,
    ) -> Result<crate::passes::output::OutputImages, GraphError> {
        self.recipe.add_output(graph, hdr, exposure)
    }
    pub fn add_scene(
        &mut self,
        graph: &mut Graph,
        materials: &Materials,
        frame: Frame<'_>,
    ) -> Result<SceneImages, GraphError> {
        let Frame {
            view,
            defaults,
            draws,
            lights,
            camera,
        } = frame;
        if draws.len() > self.limits.max_draws {
            return Err(GraphError::invalid_input(
                "mixed scene draw capacity exceeded",
            ));
        }
        self.cache.clear();
        self.materials.clear();
        self.opaque.clear();
        self.transparent.clear();
        self.casters.clear();
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
            if blend {
                let depth = draw.view_depth(view)?;
                self.order.push((depth, i));
            } else {
                self.record(graph, view, defaults, draw, instance)?;
            }
        }
        self.order
            .sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        for index in 0..self.order.len() {
            let draw = &draws[self.order[index].1];
            self.record(graph, view, defaults, draw, materials.get(draw.material)?)?;
        }
        self.forward.pbr_blends = self.transparent.len();
        let mut frame = pbr::Frame::new(
            view,
            &self.materials,
            &self.opaque,
            &self.transparent,
            &self.casters,
            defaults,
        )?;
        frame.forward_draws = Some(&self.forward);
        frame.transforms = &self.transforms;
        frame.camera_target = camera;
        self.recipe.add_scene(graph, frame, lights)
    }
    fn record(
        &mut self,
        graph: &mut Graph,
        view: &PreparedView,
        defaults: DefaultViews,
        draw: &Draw,
        instance: &MaterialInstance,
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
                    return Err(GraphError::invalid_input(
                        "mixed scene transform capacity exceeded",
                    ));
                }
                let index = self.transforms.len() as u32;
                self.transforms.push(transform);
                pbr::Draw::model(geometry, index)
            } else {
                pbr::Draw::WorldSpaceIndexed(geometry)
            };
            if blend {
                self.transparent.push(command)
            } else {
                self.opaque.push(command);
                if draw.casts_shadow && self.config.shadows.is_some() {
                    if self.casters.len() + self.forward.shadow[0].len()
                        >= self.limits.max_casters_per_cascade
                    {
                        return Err(GraphError::invalid_input(
                            "mixed scene shadow capacity exceeded",
                        ));
                    }
                    self.casters.push(command)
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
        let (program, shadow) = match instance {
            MaterialInstance::Unlit(_) => (&self.unlit, Some(&unlit_shadow)),
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
            if self.casters.len() + self.forward.shadow[0].len()
                >= self.limits.max_casters_per_cascade
            {
                return Err(GraphError::invalid_input(
                    "mixed scene shadow capacity exceeded",
                ));
            }
            for (dst, matrix) in self.forward.shadow.iter_mut().zip(matrices) {
                dst.push(command(
                    program,
                    resolved.shadow,
                    constants(product(*matrix, model))?,
                ))
            }
        }
        let cmd = command(program, resolved.color, constants(clip)?);
        if blend {
            self.forward.blend.push((self.transparent.len(), cmd))
        } else {
            self.forward.opaque.push(cmd)
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
            return Err(GraphError::invalid_input(
                "mixed scene material capacity exceeded",
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

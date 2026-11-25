use sashiko_renderer_core::graph::{
    Binding, BindingResource, BufferHandle, Graph, GraphCapacityKind, GraphError, ImageView,
    IndexedIndirectArgs, RasterBindings, RasterConstants, RasterDraw, RasterPass,
};
use sashiko_renderer_core::program::{
    MAX_RASTER_BINDINGS, ManagedRasterProgram, SamplerDescriptor,
};

use crate::passes::pbr::CAMERA_BYTES;

use super::instances::{InstanceBuffer, PAGE_RECORDS, TRANSFORM_BINDING};
use super::{AlphaMode, MaterialUniform};
use std::collections::HashMap;

/// The standard material maps. Base color and emissive use sRGB views for
/// sRGB-encoded assets. Normal and ORM use linear views. ORM stores occlusion,
/// roughness and metallic in the red, green and blue channels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaterialTextures<T = ImageView> {
    pub base_color: T,
    pub normal: T,
    pub orm: T,
    pub emissive: T,
}

/// Sampling state for each material map. Masked shadows use the base-color sampler.
pub type MaterialSamplers = MaterialTextures<SamplerDescriptor>;

impl<T: Copy> MaterialTextures<T> {
    pub const fn uniform(value: T) -> Self {
        Self {
            base_color: value,
            normal: value,
            orm: value,
            emissive: value,
        }
    }
}

/// Graph-local factors and texture views. The alpha mode comes from the same
/// upload that supplies the shader factors.
#[derive(Clone, Copy, Debug)]
pub struct GraphMaterial {
    factors: MaterialUniform,
    textures: MaterialTextures,
    samplers: MaterialSamplers,
}

impl GraphMaterial {
    /// Use linear filtering and repeat addressing on all material maps.
    pub const fn new(factors: MaterialUniform, textures: MaterialTextures) -> Self {
        Self {
            factors,
            textures,
            samplers: MaterialSamplers::uniform(SamplerDescriptor::LinearRepeat),
        }
    }

    #[must_use]
    pub const fn with_samplers(mut self, samplers: MaterialSamplers) -> Self {
        self.samplers = samplers;
        self
    }

    pub const fn alpha_mode(self) -> AlphaMode {
        self.factors.alpha_mode()
    }

    pub const fn factors(self) -> MaterialUniform {
        self.factors
    }
    pub const fn samplers(self) -> MaterialSamplers {
        self.samplers
    }
    pub const fn textures(self) -> MaterialTextures {
        self.textures
    }
}

/// An indexed triangle list in the standard 48-byte vertex format.
/// Indices are u32 and `material` indexes the current frame's material slice.
#[derive(Clone, Copy, Debug)]
pub struct IndexedDraw {
    pub vertices: BufferHandle,
    pub indices: BufferHandle,
    pub first_index: u32,
    pub index_count: u32,
    pub material: usize,
}

/// Independent mesh UVs and per-placement lighting for a standard PBR draw.
#[derive(Clone, Copy, Debug)]
pub struct DrawLightmap {
    pub vertices: BufferHandle,
    pub image: ImageView,
    slot: u32,
    packed: bool,
    pub(crate) transform: Option<u32>,
}
impl DrawLightmap {
    pub const fn new(
        vertices: BufferHandle,
        image: ImageView,
        address: super::LightmapAddress,
    ) -> Self {
        Self {
            vertices,
            image,
            slot: address.slot,
            packed: false,
            transform: None,
        }
    }
    pub const fn packed(vertices: BufferHandle, image: ImageView) -> Self {
        Self {
            vertices,
            image,
            slot: 0,
            packed: true,
            transform: None,
        }
    }
}

/// A draw using graph-local handles. Indexed and untransformed indirect
/// draws use world-space vertices. Draws with transforms use model-space
/// vertices.
#[derive(Clone, Copy, Debug)]
pub enum Draw {
    /// World-space static placement. The UV buffer is shared by mesh instances.
    Lightmapped {
        geometry: IndexedDraw,
        surface: DrawLightmap,
    },
    /// Packed opaque draws share one page image; each packed vertex carries
    /// its placement's one-based slot alongside the mesh UV.
    LightmappedIndirect {
        args: IndexedIndirectArgs,
        material: usize,
        surface: DrawLightmap,
    },
    WorldSpaceIndexed(IndexedDraw),
    /// Opaque or alpha-masked geometry. The argument contains its geometry.
    WorldSpaceIndirect {
        args: IndexedIndirectArgs,
        material: usize,
    },
    /// Opaque or alpha-masked model-space geometry. Every active command in
    /// the argument group shares one transform. Supply world-space bounds
    /// when building GPU visibility for these commands.
    TransformedIndirect {
        args: IndexedIndirectArgs,
        material: usize,
        transform: u32,
    },
    /// An ordered nonempty range in [`super::PbrDrawInput::transforms`]. Use the same
    /// range in every renderable cascade that contains these instances.
    Instanced {
        geometry: IndexedDraw,
        first_transform: u32,
        instance_count: u32,
    },
}

impl Draw {
    /// Number of recorded draw commands used against [`super::SceneLimits`].
    /// An instance range crossing transform pages is split in original order.
    pub const fn command_count(&self) -> usize {
        match *self {
            Self::Instanced {
                first_transform,
                instance_count,
                ..
            } => {
                let records = (first_transform % PAGE_RECORDS) as u64 + instance_count as u64;
                records.div_ceil(PAGE_RECORDS as u64) as usize
            }
            _ => 1,
        }
    }

    /// Draw a single model-space mesh using the indexed frame transform.
    pub const fn model(geometry: IndexedDraw, transform_index: u32) -> Self {
        Self::Instanced {
            geometry,
            first_transform: transform_index,
            instance_count: 1,
        }
    }
}

#[derive(Clone, Copy)]
enum Command {
    Surface {
        draw: RasterDraw,
        packed: bool,
    },
    SurfaceIndirect {
        args: IndexedIndirectArgs,
        bindings: RasterBindings,
        packed: bool,
        slot: u32,
    },
    Indexed(RasterDraw),
    Indirect {
        args: IndexedIndirectArgs,
        bindings: RasterBindings,
        first_transform: Option<u32>,
    },
    Instanced(RasterDraw),
}

impl Command {
    fn indexed(&self) -> Option<&RasterDraw> {
        match self {
            Self::Indexed(draw) | Self::Instanced(draw) => Some(draw),
            Self::Indirect { .. } | Self::Surface { .. } | Self::SurfaceIndirect { .. } => None,
        }
    }
}

pub(crate) struct Batch {
    bindings: Vec<Option<RasterBindings>>,
    instanced_bindings: Vec<Option<RasterBindings>>,
    // Page zero is indexed directly for small scenes. Only pairs actually used
    // on subsequent pages occupy this prepared cache.
    paged_bindings: HashMap<(usize, u32), RasterBindings>,
    surface_bindings: HashMap<(usize, ImageView, bool, Option<u32>), RasterBindings>,
    commands: Vec<Command>,
}

impl Batch {
    pub(crate) fn prepare(
        materials: usize,
        draws: usize,
        pages: usize,
        binding_uses: usize,
        surface_uses: usize,
    ) -> Result<Self, GraphError> {
        let paged_capacity = materials
            .saturating_mul(pages.saturating_sub(1))
            .min(binding_uses);
        let mut paged_bindings = HashMap::new();
        paged_bindings.try_reserve(paged_capacity).map_err(|_| {
            GraphError::capacity_error(GraphCapacityKind::Bindings, paged_capacity, None)
        })?;
        let mut surface_bindings = HashMap::new();
        surface_bindings.try_reserve(surface_uses).map_err(|_| {
            GraphError::capacity_error(GraphCapacityKind::Bindings, surface_uses, None)
        })?;
        let bindings = |count| {
            let mut slots = Vec::new();
            slots.try_reserve_exact(count).map_err(|_| {
                GraphError::capacity_error(GraphCapacityKind::Bindings, count, None)
            })?;
            slots.resize(count, None);
            Ok::<_, GraphError>(slots)
        };
        let mut commands = Vec::new();
        commands
            .try_reserve_exact(draws)
            .map_err(|_| GraphError::capacity_error(GraphCapacityKind::Draws, draws, None))?;
        Ok(Self {
            bindings: bindings(materials)?,
            instanced_bindings: bindings(if pages > 0 { materials } else { 0 })?,
            paged_bindings,
            surface_bindings,
            commands,
        })
    }

    pub(crate) fn reset_bindings(&mut self, material_count: usize) {
        self.paged_bindings.clear();
        self.surface_bindings.clear();
        self.bindings[..material_count].fill(None);
        if !self.instanced_bindings.is_empty() {
            self.instanced_bindings[..material_count].fill(None);
        }
    }

    pub(super) fn record(
        &mut self,
        graph: &mut Graph,
        program: &ManagedRasterProgram,
        stage: Bindings<'_>,
        materials: &[GraphMaterial],
        draws: &[Draw],
        instances: Option<(&ManagedRasterProgram, InstanceBuffer)>,
    ) -> Result<(), GraphError> {
        self.commands.clear();
        let parameters = stage.parameters(None)?;
        for draw in draws {
            // Frame preflight checked material indices and instance ranges.
            let command = match *draw {
                Draw::Lightmapped { geometry, surface } => {
                    let model = surface.transform.map(|index| {
                        let (program, table) =
                            instances.expect("preflight validated the model table");
                        (program, (table, index / PAGE_RECORDS), index % PAGE_RECORDS)
                    });
                    if matches!(stage, Bindings::Shadow { .. }) {
                        if let Some((program, table, first)) = model {
                            let bindings = self.model_binding(
                                graph,
                                program,
                                stage,
                                materials,
                                geometry.material,
                                table,
                            )?;
                            Command::Instanced(indexed(
                                geometry,
                                bindings,
                                1,
                                stage.parameters(Some(first))?,
                            ))
                        } else {
                            let bindings = Self::material_binding(
                                &mut self.bindings,
                                graph,
                                program,
                                stage,
                                materials,
                                geometry.material,
                            )?;
                            Command::Indexed(indexed(geometry, bindings, 1, parameters))
                        }
                    } else {
                        let selected = model.map_or(program, |m| m.0);
                        let variant =
                            crate::passes::pbr::lightmapped_program(selected, surface.packed)
                                .ok_or_else(|| {
                                    GraphError::invalid_input("unsupported lightmap program")
                                })?;
                        let key = (
                            geometry.material,
                            surface.image,
                            surface.packed,
                            model.map(|m| m.1.1),
                        );
                        let bindings = match self.surface_bindings.entry(key) {
                            std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                            std::collections::hash_map::Entry::Vacant(e) => {
                                *e.insert(stage.bind_surface(
                                    graph,
                                    &variant,
                                    materials[geometry.material],
                                    Some(surface.image),
                                    model.map(|m| m.1),
                                )?)
                            }
                        };
                        let params = if let Some((_, _, first)) = model {
                            RasterConstants::new(&[first, surface.slot])?
                        } else {
                            RasterConstants::new(&[surface.slot])?
                        };
                        let mut draw = indexed(geometry, bindings, 1, params);
                        draw.secondary_vertices = Some(surface.vertices);
                        Command::Surface {
                            draw,
                            packed: surface.packed,
                        }
                    }
                }
                Draw::LightmappedIndirect {
                    args,
                    material,
                    surface,
                } => {
                    if matches!(stage, Bindings::Shadow { .. }) {
                        let bindings = Self::material_binding(
                            &mut self.bindings,
                            graph,
                            program,
                            stage,
                            materials,
                            material,
                        )?;
                        Command::Indirect {
                            args,
                            bindings,
                            first_transform: None,
                        }
                    } else {
                        let variant =
                            crate::passes::pbr::lightmapped_program(program, surface.packed)
                                .ok_or_else(|| {
                                    GraphError::invalid_input("unsupported lightmap program")
                                })?;
                        let key = (material, surface.image, surface.packed, None);
                        let bindings = match self.surface_bindings.entry(key) {
                            std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                            std::collections::hash_map::Entry::Vacant(e) => {
                                *e.insert(stage.bind_surface(
                                    graph,
                                    &variant,
                                    materials[material],
                                    Some(surface.image),
                                    None,
                                )?)
                            }
                        };
                        Command::SurfaceIndirect {
                            args: args.with_secondary_vertices(surface.vertices),
                            bindings,
                            packed: surface.packed,
                            slot: surface.slot,
                        }
                    }
                }
                Draw::WorldSpaceIndexed(draw) => {
                    let bindings = Self::material_binding(
                        &mut self.bindings,
                        graph,
                        program,
                        stage,
                        materials,
                        draw.material,
                    )?;
                    Command::Indexed(indexed(draw, bindings, 1, parameters))
                }
                Draw::WorldSpaceIndirect { args, material } => {
                    let bindings = Self::material_binding(
                        &mut self.bindings,
                        graph,
                        program,
                        stage,
                        materials,
                        material,
                    )?;
                    Command::Indirect {
                        args,
                        bindings,
                        first_transform: None,
                    }
                }
                Draw::TransformedIndirect {
                    args,
                    material,
                    transform,
                } => {
                    let (program, table) =
                        instances.expect("frame preflight guarantees the indirect transform table");
                    let (page, first) = (transform / PAGE_RECORDS, transform % PAGE_RECORDS);
                    let bindings = self.model_binding(
                        graph,
                        program,
                        stage,
                        materials,
                        material,
                        (table, page),
                    )?;
                    Command::Indirect {
                        args,
                        bindings,
                        first_transform: Some(first),
                    }
                }
                Draw::Instanced {
                    geometry,
                    mut first_transform,
                    mut instance_count,
                } => {
                    let (program, table) =
                        instances.expect("frame preflight guarantees the instance transform table");
                    // Instance IDs restart at zero for each page. The native binding
                    // is rebased; shaders always address the same bounded range.
                    loop {
                        let (page, first) = (
                            first_transform / PAGE_RECORDS,
                            first_transform % PAGE_RECORDS,
                        );
                        let count = instance_count.min(PAGE_RECORDS - first);
                        let bindings = self.model_binding(
                            graph,
                            program,
                            stage,
                            materials,
                            geometry.material,
                            (table, page),
                        )?;
                        let command = Command::Instanced(indexed(
                            geometry,
                            bindings,
                            count,
                            stage.parameters(Some(first))?,
                        ));
                        if count == instance_count {
                            break command;
                        }
                        self.commands.push(command);
                        first_transform += count;
                        instance_count -= count;
                    }
                }
            };
            self.commands.push(command);
        }
        Ok(())
    }

    fn material_binding(
        cache: &mut [Option<RasterBindings>],
        graph: &mut Graph,
        program: &ManagedRasterProgram,
        stage: Bindings<'_>,
        materials: &[GraphMaterial],
        material: usize,
    ) -> Result<RasterBindings, GraphError> {
        let slot = &mut cache[material];
        if let Some(bindings) = *slot {
            return Ok(bindings);
        }
        let bindings = stage.bind(graph, program, materials[material], None)?;
        *slot = Some(bindings);
        Ok(bindings)
    }

    fn model_binding(
        &mut self,
        graph: &mut Graph,
        program: &ManagedRasterProgram,
        stage: Bindings<'_>,
        materials: &[GraphMaterial],
        material: usize,
        (table, page): (InstanceBuffer, u32),
    ) -> Result<RasterBindings, GraphError> {
        if page == 0 {
            let slot = &mut self.instanced_bindings[material];
            if let Some(bindings) = *slot {
                return Ok(bindings);
            }
            let bindings = stage.bind(graph, program, materials[material], Some((table, page)))?;
            *slot = Some(bindings);
            return Ok(bindings);
        }
        match self.paged_bindings.entry((material, page)) {
            std::collections::hash_map::Entry::Occupied(entry) => Ok(*entry.get()),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let bindings =
                    stage.bind(graph, program, materials[material], Some((table, page)))?;
                Ok(*entry.insert(bindings))
            }
        }
    }

    pub(crate) fn draw(
        &self,
        pass: &mut RasterPass<'_, '_>,
        program: &ManagedRasterProgram,
        instances: Option<&ManagedRasterProgram>,
    ) -> Result<(), GraphError> {
        self.draw_range(pass, program, instances, 0..self.commands.len())
    }

    pub(crate) fn draw_range(
        &self,
        pass: &mut RasterPass<'_, '_>,
        program: &ManagedRasterProgram,
        instances: Option<&ManagedRasterProgram>,
        range: std::ops::Range<usize>,
    ) -> Result<(), GraphError> {
        if range.is_empty() {
            return pass.draw(program, std::iter::empty::<RasterDraw>());
        }
        for batch in self.commands[range]
            .chunk_by(|a, b| std::mem::discriminant(a) == std::mem::discriminant(b))
        {
            match batch[0] {
                Command::Surface { .. } | Command::SurfaceIndirect { .. } => {
                    for &cmd in batch {
                        match cmd {
                            Command::Surface { draw, packed } => {
                                let base = if draw.parameters.words().len() == 2 {
                                    instances.expect("prepared model program")
                                } else {
                                    program
                                };
                                pass.draw(
                                    &crate::passes::pbr::lightmapped_program(base, packed).unwrap(),
                                    [draw],
                                )?;
                            }
                            Command::SurfaceIndirect {
                                args,
                                bindings,
                                packed,
                                slot,
                            } => pass.draw_indirect(
                                &crate::passes::pbr::lightmapped_program(program, packed).unwrap(),
                                args,
                                Some(bindings),
                                RasterConstants::new(&[slot])?,
                            )?,
                            _ => unreachable!(),
                        }
                    }
                }
                Command::Indexed(_) => {
                    pass.draw_generated(program, batch.len(), |i| {
                        batch[i].indexed().expect("ordered indexed run")
                    })?;
                }
                Command::Instanced(_) => {
                    let program = instances.expect("instanced draws have a prepared program");
                    pass.draw_generated(program, batch.len(), |i| {
                        batch[i].indexed().expect("ordered indexed run")
                    })?;
                }
                Command::Indirect { .. } => {
                    for &command in batch {
                        let Command::Indirect {
                            args,
                            bindings,
                            first_transform,
                        } = command
                        else {
                            unreachable!("the ordered run contains only indirect draws");
                        };
                        let (program, parameters) = match first_transform {
                            Some(first) => (
                                instances
                                    .expect("transformed indirect draws have a prepared program"),
                                RasterConstants::new(&[first])?,
                            ),
                            None => (program, RasterConstants::default()),
                        };
                        pass.draw_indirect(program, args, Some(bindings), parameters)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn indexed(
    draw: IndexedDraw,
    bindings: RasterBindings,
    instance_count: u32,
    parameters: RasterConstants,
) -> RasterDraw {
    RasterDraw {
        bindings: Some(bindings),
        secondary_vertices: None,
        vertices: draw.vertices,
        indices: draw.indices,
        first_index: draw.first_index,
        index_count: draw.index_count,
        instance_count,
        parameters,
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Bindings<'a> {
    Scene {
        camera: BufferHandle,
        lighting: &'a [Binding],
    },
    Shadow {
        camera: BufferHandle,
        cascade: usize,
    },
}

impl Bindings<'_> {
    fn parameters(self, first_transform: Option<u32>) -> Result<RasterConstants, GraphError> {
        match (self, first_transform) {
            (Self::Scene { .. }, None) => Ok(RasterConstants::default()),
            (Self::Scene { .. }, Some(first)) => RasterConstants::new(&[first]),
            (Self::Shadow { cascade, .. }, None) => RasterConstants::new(&[cascade as u32]),
            (Self::Shadow { cascade, .. }, Some(first)) => {
                RasterConstants::new(&[cascade as u32, first])
            }
        }
    }

    fn bind(
        self,
        graph: &mut Graph,
        program: &ManagedRasterProgram,
        material: GraphMaterial,
        transforms: Option<(InstanceBuffer, u32)>,
    ) -> Result<RasterBindings, GraphError> {
        self.bind_surface(graph, program, material, None, transforms)
    }
    fn bind_surface(
        self,
        graph: &mut Graph,
        program: &ManagedRasterProgram,
        material: GraphMaterial,
        surface: Option<ImageView>,
        transforms: Option<(InstanceBuffer, u32)>,
    ) -> Result<RasterBindings, GraphError> {
        let camera = match self {
            Self::Scene { camera, .. } | Self::Shadow { camera, .. } => camera,
        };
        let camera = buffer_binding(0, camera, 0, CAMERA_BYTES as u64);
        material.factors.check_graph(graph)?;
        let factors = material.factors.binding();
        let textures = material.textures;
        let base = image_binding(1, 0, textures.base_color);
        let sampler = sampler_binding(1, 4, material.samplers.base_color);
        let mut bindings = [camera; MAX_RASTER_BINDINGS];
        let mut count = match self {
            Self::Scene { lighting, .. } => {
                bindings[..10].copy_from_slice(&[
                    camera,
                    factors,
                    base,
                    image_binding(1, 1, textures.normal),
                    image_binding(1, 2, textures.orm),
                    image_binding(1, 3, textures.emissive),
                    sampler,
                    sampler_binding(1, 5, material.samplers.normal),
                    sampler_binding(1, 6, material.samplers.orm),
                    sampler_binding(1, 7, material.samplers.emissive),
                ]);
                let count = 10 + lighting.len();
                bindings[10..count].copy_from_slice(lighting);
                if let Some(image) = surface {
                    bindings[10] = image_binding(2, 0, image);
                }
                count
            }
            Self::Shadow { .. } => {
                bindings[..4].copy_from_slice(&[camera, factors, base, sampler]);
                4
            }
        };
        if let Some((table, page)) = transforms {
            bindings[count] = buffer_binding(
                TRANSFORM_BINDING,
                table.handle,
                table.page_offset(page),
                table.page_bytes,
            );
            count += 1;
        }
        graph.raster_bindings(program, &bindings[..count])
    }
}

fn buffer_binding(binding: u32, buffer: BufferHandle, offset: u64, size: u64) -> Binding {
    Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset,
            size,
        },
    }
}

pub(crate) fn image_binding(group: u32, binding: u32, view: ImageView) -> Binding {
    Binding {
        group,
        binding,
        resource: BindingResource::ImageView(view),
    }
}

pub(crate) fn sampler_binding(group: u32, binding: u32, sampler: SamplerDescriptor) -> Binding {
    Binding {
        group,
        binding,
        resource: BindingResource::Sampler(sampler),
    }
}

use renderer::graph::{
    Binding, BindingResource, BufferHandle, Graph, GraphCapacityKind, GraphError, ImageView,
    IndexedIndirectArgs, RasterBindings, RasterConstants, RasterDraw, RasterPass,
};
use renderer::program::{MAX_RASTER_BINDINGS, ManagedRasterProgram, SamplerDescriptor};

use crate::passes::pbr::CAMERA_BYTES;

use super::instances::{InstanceBuffer, TRANSFORM_BINDING};
use super::{AlphaMode, MaterialUniform};

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

/// Graph-local factors and texture views. The alpha mode comes from the same
/// upload that supplies the shader factors.
#[derive(Clone, Copy, Debug)]
pub struct Material {
    factors: MaterialUniform,
    textures: MaterialTextures,
    sampler: SamplerDescriptor,
}

impl Material {
    /// Use linear filtering and repeat addressing on all material maps.
    pub const fn new(factors: MaterialUniform, textures: MaterialTextures) -> Self {
        Self {
            factors,
            textures,
            sampler: SamplerDescriptor::LinearRepeat,
        }
    }

    /// Use one sampler for all maps in scene drawing, including the base-color
    /// alpha sample used by the shadow pass.
    #[must_use]
    pub const fn with_sampler(mut self, sampler: SamplerDescriptor) -> Self {
        self.sampler = sampler;
        self
    }

    pub const fn alpha_mode(self) -> AlphaMode {
        self.factors.alpha_mode()
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

/// A draw using graph-local handles. Indexed and untransformed indirect
/// draws use world-space vertices. Draws with transforms use model-space
/// vertices.
#[derive(Clone, Copy, Debug)]
pub enum Draw {
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
    /// An ordered nonempty range in [`super::Frame::transforms`]. Use the same
    /// range in every renderable cascade that contains these instances.
    Instanced {
        geometry: IndexedDraw,
        first_transform: u32,
        instance_count: u32,
    },
}

impl Draw {
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
            Self::Indirect { .. } => None,
        }
    }
}

pub(crate) struct Batch {
    bindings: Vec<Option<RasterBindings>>,
    instanced_bindings: Vec<Option<RasterBindings>>,
    commands: Vec<Command>,
}

impl Batch {
    pub(crate) fn prepare(
        materials: usize,
        draws: usize,
        transforms: bool,
    ) -> Result<Self, GraphError> {
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
            instanced_bindings: bindings(if transforms { materials } else { 0 })?,
            commands,
        })
    }

    pub(crate) fn reset_bindings(&mut self, material_count: usize) {
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
        materials: &[Material],
        draws: &[Draw],
        instances: Option<(&ManagedRasterProgram, InstanceBuffer)>,
    ) -> Result<(), GraphError> {
        self.commands.clear();
        let parameters = stage.parameters(None)?;
        for draw in draws {
            // Frame preflight checked material indices and instance ranges.
            let command = match *draw {
                Draw::WorldSpaceIndexed(draw) => {
                    let bindings = Self::material_binding(
                        &mut self.bindings,
                        graph,
                        program,
                        stage,
                        materials,
                        draw.material,
                        None,
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
                        None,
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
                    let bindings = Self::material_binding(
                        &mut self.instanced_bindings,
                        graph,
                        program,
                        stage,
                        materials,
                        material,
                        Some(table),
                    )?;
                    Command::Indirect {
                        args,
                        bindings,
                        first_transform: Some(transform),
                    }
                }
                Draw::Instanced {
                    geometry,
                    first_transform,
                    instance_count,
                } => {
                    let (program, table) =
                        instances.expect("frame preflight guarantees the instance transform table");
                    let bindings = Self::material_binding(
                        &mut self.instanced_bindings,
                        graph,
                        program,
                        stage,
                        materials,
                        geometry.material,
                        Some(table),
                    )?;
                    Command::Instanced(indexed(
                        geometry,
                        bindings,
                        instance_count,
                        stage.parameters(Some(first_transform))?,
                    ))
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
        materials: &[Material],
        material: usize,
        transforms: Option<InstanceBuffer>,
    ) -> Result<RasterBindings, GraphError> {
        let slot = &mut cache[material];
        if let Some(bindings) = *slot {
            return Ok(bindings);
        }
        let bindings = stage.bind(graph, program, materials[material], transforms)?;
        *slot = Some(bindings);
        Ok(bindings)
    }

    pub(crate) fn draw(
        &self,
        pass: &mut RasterPass<'_, '_>,
        program: &ManagedRasterProgram,
        instances: Option<&ManagedRasterProgram>,
    ) -> Result<(), GraphError> {
        if self.commands.is_empty() {
            return pass.draw(program, std::iter::empty::<RasterDraw>());
        }
        for batch in self
            .commands
            .chunk_by(|a, b| std::mem::discriminant(a) == std::mem::discriminant(b))
        {
            match batch[0] {
                Command::Indexed(_) => {
                    pass.draw(program, batch.iter().filter_map(Command::indexed))?;
                }
                Command::Instanced(_) => {
                    let program = instances.expect("instanced draws have a prepared program");
                    pass.draw(program, batch.iter().filter_map(Command::indexed))?;
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
        material: Material,
        transforms: Option<InstanceBuffer>,
    ) -> Result<RasterBindings, GraphError> {
        let camera = match self {
            Self::Scene { camera, .. } | Self::Shadow { camera, .. } => camera,
        };
        let camera = buffer_binding(0, camera, 0, CAMERA_BYTES as u64);
        material.factors.check_graph(graph)?;
        let factors = material.factors.binding();
        let textures = material.textures;
        let base = image_binding(1, 0, textures.base_color);
        let sampler = sampler_binding(1, 4, material.sampler);
        let mut bindings = [camera; MAX_RASTER_BINDINGS];
        let mut count = match self {
            Self::Scene { lighting, .. } => {
                bindings[..7].copy_from_slice(&[
                    camera,
                    factors,
                    base,
                    image_binding(1, 1, textures.normal),
                    image_binding(1, 2, textures.orm),
                    image_binding(1, 3, textures.emissive),
                    sampler,
                ]);
                let count = 7 + lighting.len();
                bindings[7..count].copy_from_slice(lighting);
                count
            }
            Self::Shadow { .. } => {
                bindings[..4].copy_from_slice(&[camera, factors, base, sampler]);
                4
            }
        };
        if let Some(transforms) = transforms {
            bindings[count] =
                buffer_binding(TRANSFORM_BINDING, transforms.handle, 0, transforms.size);
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

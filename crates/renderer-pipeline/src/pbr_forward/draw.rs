use renderer::graph::{
    Binding, BindingResource, BufferHandle, Graph, GraphCapacityKind, GraphError, ImageView,
    RasterBindings, RasterConstants, RasterDraw,
};
use renderer::program::{ManagedRasterProgram, SamplerDescriptor};

use crate::passes::pbr::{CAMERA_BYTES, MATERIAL_BYTES};

/// A material's graph-local factor block and texture views in base color,
/// normal, ORM and emissive order. Base color and emissive are sRGB textures.
/// Use [`super::MaterialFactors`] to encode factors at the asset boundary.
#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub factors: BufferHandle,
    pub factor_offset: u64,
    pub textures: [ImageView; 4],
}

/// A triangle-list draw using world-space positions, normals, tangents and UVs
/// in the standard 48-byte vertex layout and u32 indices. Both handles belong
/// to the current Graph. `material` indexes this frame's material slice.
#[derive(Clone, Copy, Debug)]
pub struct Draw {
    pub vertices: BufferHandle,
    pub indices: BufferHandle,
    pub first_index: u32,
    pub index_count: u32,
    pub material: usize,
}

pub(super) struct Batch {
    bindings: Vec<Option<RasterBindings>>,
    pub(super) draws: Vec<RasterDraw>,
}

impl Batch {
    pub(super) fn prepare(materials: usize, draws: usize) -> Result<Self, GraphError> {
        let mut bindings = Vec::new();
        bindings.try_reserve_exact(materials).map_err(|_| {
            GraphError::capacity_error(GraphCapacityKind::Bindings, materials, None)
        })?;
        bindings.resize(materials, None);
        let mut records = Vec::new();
        records
            .try_reserve_exact(draws)
            .map_err(|_| GraphError::capacity_error(GraphCapacityKind::Draws, draws, None))?;
        Ok(Self {
            bindings,
            draws: records,
        })
    }

    pub(super) fn reset_bindings(&mut self, material_count: usize) {
        self.bindings[..material_count].fill(None);
    }

    pub(super) fn record(
        &mut self,
        graph: &mut Graph,
        program: &ManagedRasterProgram,
        stage: Bindings<'_>,
        materials: &[Material],
        draws: &[Draw],
    ) -> Result<(), GraphError> {
        self.draws.clear();
        let parameters = match stage {
            Bindings::Shadow { cascade, .. } => RasterConstants::new(&[cascade as u32])?,
            Bindings::Scene { .. } => RasterConstants::default(),
        };
        for draw in draws {
            let material = materials.get(draw.material).ok_or_else(|| {
                GraphError::invalid_input("PBR draw material index is out of bounds")
            })?;
            let slot = &mut self.bindings[draw.material];
            let bindings = match *slot {
                Some(bindings) => bindings,
                None => {
                    let bindings = stage.bind(graph, program, *material)?;
                    *slot = Some(bindings);
                    bindings
                }
            };
            self.draws.push(RasterDraw {
                bindings: Some(bindings),
                vertices: draw.vertices,
                indices: draw.indices,
                first_index: draw.first_index,
                index_count: draw.index_count,
                instance_count: 1,
                parameters,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) enum Bindings<'a> {
    Scene {
        camera: BufferHandle,
        lighting: &'a [Binding; 9],
    },
    Shadow {
        camera: BufferHandle,
        cascade: usize,
    },
}

impl Bindings<'_> {
    fn bind(
        self,
        graph: &mut Graph,
        program: &ManagedRasterProgram,
        material: Material,
    ) -> Result<RasterBindings, GraphError> {
        let camera = match self {
            Self::Scene { camera, .. } | Self::Shadow { camera, .. } => camera,
        };
        let camera = buffer_binding(0, camera, 0, CAMERA_BYTES as u64);
        let factors = buffer_binding(
            1,
            material.factors,
            material.factor_offset,
            MATERIAL_BYTES as u64,
        );
        let [base, normal, orm, emissive] = material.textures;
        let base = image_binding(1, 0, base);
        let sampler = sampler_binding(1, 4, SamplerDescriptor::LinearRepeat);
        match self {
            Self::Scene { lighting, .. } => {
                let mut bindings = [camera; 16];
                bindings[..7].copy_from_slice(&[
                    camera,
                    factors,
                    base,
                    image_binding(1, 1, normal),
                    image_binding(1, 2, orm),
                    image_binding(1, 3, emissive),
                    sampler,
                ]);
                bindings[7..].copy_from_slice(lighting);
                graph.raster_bindings(program, &bindings)
            }
            Self::Shadow { .. } => {
                graph.raster_bindings(program, &[camera, factors, base, sampler])
            }
        }
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

pub(super) fn image_binding(group: u32, binding: u32, view: ImageView) -> Binding {
    Binding {
        group,
        binding,
        resource: BindingResource::ImageView(view),
    }
}

pub(super) fn sampler_binding(group: u32, binding: u32, sampler: SamplerDescriptor) -> Binding {
    Binding {
        group,
        binding,
        resource: BindingResource::Sampler(sampler),
    }
}

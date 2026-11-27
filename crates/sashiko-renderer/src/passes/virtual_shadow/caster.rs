use super::{
    AssignedPages, CAMERA_BYTES, LEVELS, PHYSICAL_PAGES, PageRequests, SLOT_BYTES, TABLE_BYTES,
};
use crate::{
    passes::pbr::SHADOW,
    pbr::{AlphaMode, GraphMaterial, Transform},
    pipeline::Mesh,
};
use sashiko_renderer_core::{
    graph::{
        Binding, BindingResource, Graph, GraphError, RasterBindings, RasterConstants, RasterDraw,
        RasterPass,
    },
    program::{
        BindingLayout, BindingType, ManagedRasterProgram, RasterParameterLayout,
        RasterProgramDescriptor, ShaderStages,
    },
};

#[allow(unsafe_code)]
pub(super) fn program() -> ManagedRasterProgram {
    const fn vertex(binding: u32, kind: BindingType, size: usize) -> BindingLayout {
        BindingLayout {
            group: 0,
            binding,
            kind,
            min_size: size as u64,
            stages: ShaderStages::VERTEX,
        }
    }
    static DESC: RasterProgramDescriptor = RasterProgramDescriptor {
        parameters: RasterParameterLayout {
            size: 68,
            stages: ShaderStages::VERTEX,
        },
        bindings: &[
            vertex(0, BindingType::Uniform, CAMERA_BYTES),
            SHADOW.bindings[1],
            vertex(2, BindingType::StorageRead, SLOT_BYTES),
            vertex(3, BindingType::StorageRead, TABLE_BYTES),
            SHADOW.bindings[2],
            SHADOW.bindings[3],
        ],
        binding_bytes: &[
            CAMERA_BYTES as u64,
            64,
            SLOT_BYTES as u64,
            TABLE_BYTES as u64,
            0,
            0,
        ],
        shaders: sashiko_renderer_core::program::embedded_shader_binaries!(
            sashiko_renderer_core::program::RasterShaderStages {
                vertex: include_bytes!("../../../shaders/virtual_shadow/caster.vert.spv"),
                fragment: Some(include_bytes!(
                    "../../../shaders/virtual_shadow/caster.frag.spv"
                )),
            },
            sashiko_renderer_core::program::D3D12RasterShaders {
                stages: sashiko_renderer_core::program::RasterShaderStages {
                    vertex: include_bytes!("../../../shaders/virtual_shadow/dxil/caster.vert.dxil"),
                    fragment: Some(include_bytes!(
                        "../../../shaders/virtual_shadow/dxil/caster.frag.dxil"
                    )),
                },
                inline_constants: Some(
                    sashiko_renderer_core::program::D3D12InlineConstantBinding {
                        register: 7,
                        space: 0
                    }
                ),
            },
        ),
        ..SHADOW
    };
    // SAFETY: same stream, alpha texture lookup, sidedness and fixed-function
    // depth outputs as standard shadows. Integer bounds precede all shader
    // loads, including indirections under arbitrary slot/table contents.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&DESC) }
}

/// Draw state for a mesh intersecting the current light grid. Store these
/// records in prepared caller storage before opening the atlas raster pass.
pub struct CasterDraw {
    mesh: Mesh,
    bindings: RasterBindings,
    transform: Transform,
    rectangles: [Option<[u32; 4]>; LEVELS],
}

/// Prepare one standard PBR mesh range, including casters outside the camera.
pub fn bind_caster(
    graph: &mut Graph,
    requests: &PageRequests,
    pages: &AssignedPages,
    mesh: Mesh,
    transform: Transform,
    material: GraphMaterial,
) -> Result<Option<CasterDraw>, GraphError> {
    if material.alpha_mode() == AlphaMode::Blend {
        return Ok(None);
    }
    if mesh.vertex_stride != SHADOW.vertex_stride {
        return Err(GraphError::invalid_input(
            "virtual shadows require the PBR vertex stream",
        ));
    }
    let rectangles = rectangles(requests, transform.bounds(mesh.bounds));
    if rectangles.iter().all(Option::is_none) {
        return Ok(None);
    }
    let buf = |binding, buffer, size: usize| Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size: size as u64,
        },
    };
    let bindings = material.factors().raster_bindings(
        graph,
        &program(),
        &[
            buf(0, requests.camera, CAMERA_BYTES),
            buf(2, pages.slots, SLOT_BYTES),
            buf(3, pages.table, TABLE_BYTES),
            Binding {
                group: 1,
                binding: 0,
                resource: BindingResource::ImageView(material.textures().base_color),
            },
            Binding {
                group: 1,
                binding: 4,
                resource: BindingResource::Sampler(material.samplers().base_color),
            },
        ],
    )?;
    Ok(Some(CasterDraw {
        mesh,
        bindings,
        transform,
        rectangles,
    }))
}

impl CasterDraw {
    /// Emit one instance per candidate page, bounded by the physical pool.
    pub fn record(&self, pass: &mut RasterPass<'_, '_>) -> Result<(), GraphError> {
        let mut words = [0u32; 17];
        words[..12].copy_from_slice(self.transform.model_key().as_flattened());
        words[16] = u32::from(self.transform.mirrored());
        for (level, rect) in self.rectangles.iter().enumerate() {
            let Some([x, y, width, height]) = *rect else {
                continue;
            };
            let count = width * height;
            let width = if count > PHYSICAL_PAGES as u32 {
                0
            } else {
                width
            };
            words[12..16].copy_from_slice(&[level as u32, x, y, width]);
            pass.draw(
                &program(),
                [RasterDraw {
                    vertices: self.mesh.vertices,
                    indices: self.mesh.indices,
                    first_index: self.mesh.first_index,
                    index_count: self.mesh.index_count,
                    instance_count: count.min(PHYSICAL_PAGES as u32),
                    bindings: Some(self.bindings),
                    secondary_vertices: None,
                    parameters: RasterConstants::new(&words)?,
                }],
            )?;
        }
        Ok(())
    }
}

pub(super) fn rectangles(
    requests: &PageRequests,
    world: Option<crate::pbr::Bounds>,
) -> [Option<[u32; 4]>; LEVELS] {
    let projected = world.map(|bounds| {
        requests.axes.map(|axis| {
            let mut lo = 0.;
            let mut hi = 0.;
            let mut magnitude = 0.;
            for (a, (min, max)) in axis
                .into_iter()
                .zip(bounds.min().into_iter().zip(bounds.max()))
            {
                let (x, y) = (a * f64::from(min), a * f64::from(max));
                lo += x.min(y);
                hi += x.max(y);
                magnitude += x.abs().max(y.abs());
            }
            let margin = magnitude * 16. * f64::from(f32::EPSILON) + f64::from(f32::MIN_POSITIVE);
            [lo - margin, hi + margin]
        })
    });
    std::array::from_fn(|level| {
        let Some(bounds) = projected else {
            return Some([0, 0, 128, 128]);
        };
        let [x, y] = std::array::from_fn(|i| {
            bounds[i].map(|v| {
                (v * (25. / (1u32 << level) as f64)).floor() - f64::from(requests.origins[level][i])
            })
        });
        if x[1] < 0. || y[1] < 0. || x[0] >= 128. || y[0] >= 128. {
            return None;
        }
        let [x0, x1] = x.map(|v| v.clamp(0., 127.) as u32);
        let [y0, y1] = y.map(|v| v.clamp(0., 127.) as u32);
        Some([x0, y0, x1 - x0 + 1, y1 - y0 + 1])
    })
}

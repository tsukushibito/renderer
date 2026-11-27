use super::{AssignedPages, CAMERA_BYTES, LEVELS, PHYSICAL_PAGES, PageRequests, SLOT_BYTES, cull};
use crate::{
    passes::pbr::SHADOW,
    pbr::{AlphaMode, GraphMaterial, Transform},
    pipeline::Mesh,
};
use sashiko_renderer_core::{
    graph::{
        Binding, BindingResource, BufferHandle, Graph, GraphCapacityKind, GraphError, ImageHandle,
        IndexedIndirectTemplate, IndexedInstanceBatch, RasterBindings, RasterConstants,
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
            size: 52,
            stages: ShaderStages::VERTEX,
        },
        bindings: &[
            vertex(0, BindingType::Uniform, CAMERA_BYTES),
            SHADOW.bindings[1],
            vertex(2, BindingType::StorageRead, SLOT_BYTES),
            vertex(3, BindingType::StorageRead, cull::LIST_BYTES),
            SHADOW.bindings[2],
            SHADOW.bindings[3],
        ],
        binding_bytes: &[
            CAMERA_BYTES as u64,
            64,
            SLOT_BYTES as u64,
            cull::LIST_BYTES as u64,
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

/// Prepared host storage for GPU-culling and drawing independent caster meshes.
pub struct CasterBatch {
    capacity: usize,
    instances: IndexedInstanceBatch,
    rectangles: Vec<u8>,
    materials: Vec<(GraphMaterial, RasterConstants)>,
    bindings: Vec<RasterBindings>,
}

impl CasterBatch {
    pub fn new(capacity: usize) -> Result<Self, GraphError> {
        let instances = IndexedInstanceBatch::new(capacity)?;
        let padded = capacity.next_multiple_of(64);
        padded
            .checked_mul(cull::LIST_BYTES)
            .ok_or_else(|| GraphError::invalid_input("virtual caster list size overflow"))?;
        Ok(Self {
            capacity,
            instances,
            rectangles: Vec::with_capacity(padded * cull::RECT_BYTES),
            materials: Vec::with_capacity(capacity),
            bindings: Vec::with_capacity(capacity),
        })
    }

    pub fn clear(&mut self) {
        self.instances.clear();
        self.rectangles.clear();
        self.materials.clear();
        self.bindings.clear();
    }

    pub fn add(
        &mut self,
        requests: &PageRequests,
        mesh: Mesh,
        transform: Transform,
        material: GraphMaterial,
    ) -> Result<(), GraphError> {
        if material.alpha_mode() == AlphaMode::Blend {
            return Ok(());
        }
        if mesh.vertex_stride != SHADOW.vertex_stride {
            return Err(GraphError::invalid_input(
                "virtual shadows require the PBR vertex stream",
            ));
        }
        let bounds = rectangles(requests, transform.bounds(mesh.bounds));
        if bounds.iter().all(Option::is_none) {
            return Ok(());
        }
        if self.materials.len() >= self.capacity {
            return Err(GraphError::capacity_error(
                GraphCapacityKind::Draws,
                self.capacity,
                self.capacity.checked_add(1),
            ));
        }
        let mut words = [0u32; 13];
        words[..12].copy_from_slice(transform.model_key().as_flattened());
        words[12] = u32::from(transform.mirrored());
        let constants = RasterConstants::new(&words)?;
        self.instances.push(
            mesh.vertices,
            mesh.indices,
            IndexedIndirectTemplate {
                first_index: mesh.first_index,
                index_count: mesh.index_count,
            },
            PHYSICAL_PAGES as u32,
        )?;
        self.rectangles
            .truncate(self.materials.len() * cull::RECT_BYTES);
        for rect in bounds {
            let [x, y, w, h] = rect.unwrap_or([0; 4]);
            self.rectangles
                .extend_from_slice([x, y, x + w, y + h].map(u32::to_le_bytes).as_flattened());
        }
        self.materials.push((material, constants));
        Ok(())
    }

    pub fn draw(
        &mut self,
        graph: &mut Graph,
        requests: &PageRequests,
        pages: &AssignedPages,
        atlas: ImageHandle,
        preserve: bool,
    ) -> Result<(), GraphError> {
        if self.materials.is_empty() {
            return super::draw_atlas(graph, atlas, pages, preserve, |_| Ok(()));
        }
        let padded = self.materials.len().next_multiple_of(64);
        self.rectangles.resize(padded * cull::RECT_BYTES, 0);
        let (lists, counts) = cull::record(graph, requests, pages, &self.rectangles)?;
        let encoded = self.instances.encode(graph, cull::ENCODE, counts)?;
        self.bindings.clear();
        let program = program();
        for (i, (material, _)) in self.materials.iter().enumerate() {
            self.bindings
                .push(bind(graph, &program, requests, pages, *material, lists, i)?);
        }
        super::draw_atlas(graph, atlas, pages, preserve, |pass| {
            for ((args, &bindings), &(_, constants)) in
                encoded.commands().zip(&self.bindings).zip(&self.materials)
            {
                pass.draw_indirect(&program, args, Some(bindings), constants)?;
            }
            Ok(())
        })
    }
}

fn bind(
    graph: &mut Graph,
    program: &ManagedRasterProgram,
    requests: &PageRequests,
    pages: &AssignedPages,
    material: GraphMaterial,
    lists: BufferHandle,
    index: usize,
) -> Result<RasterBindings, GraphError> {
    let buffer = |binding, handle, offset, size: usize| Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer: handle,
            offset: offset as u64,
            size: size as u64,
        },
    };
    material.factors().raster_bindings(
        graph,
        program,
        &[
            buffer(0, requests.camera, 0, CAMERA_BYTES),
            buffer(2, pages.slots, 0, SLOT_BYTES),
            buffer(3, lists, index * cull::LIST_BYTES, cull::LIST_BYTES),
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
    )
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
            // Expand both axes so geometry outside the interior also fills the
            // four edges and corners. Invalidation uses this same projection.
            let scale = 25. / (1u32 << level) as f64;
            let gutter = super::GUTTER as f64 / super::PAGE_TEXELS as f64;
            [bounds[i][0] * scale - gutter, bounds[i][1] * scale + gutter]
                .map(|v| v.floor() - f64::from(requests.origins[level][i]))
        });
        if x[1] < 0. || y[1] < 0. || x[0] >= 128. || y[0] >= 128. {
            return None;
        }
        let [x0, x1] = x.map(|v| v.clamp(0., 127.) as u32);
        let [y0, y1] = y.map(|v| v.clamp(0., 127.) as u32);
        Some([x0, y0, x1 - x0 + 1, y1 - y0 + 1])
    })
}

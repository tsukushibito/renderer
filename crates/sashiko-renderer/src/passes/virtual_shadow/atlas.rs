use super::{AssignedPages, PHYSICAL_PAGES, SLOT_BYTES};
use sashiko_renderer_core::{
    capacity::{GraphRequirements, Preparation},
    graph::{
        Attachment, Binding, BindingResource, BufferDesc, Graph, GraphError, ImageDesc,
        ImageHandle, LoadOp, RasterAttachments, RasterConstants, RasterDraw, RasterPass,
        SampleCount, StoreOp,
    },
    program::{
        BindingLayout, BindingType, BlendState, CompareOp, DepthState, ManagedRasterProgram,
        RasterParameterLayout, RasterProgramDescriptor, ShaderStages, VertexAttribute,
        VertexFormat,
    },
};

pub const ATLAS_DESC: ImageDesc = ImageDesc::depth32(4096, 4096);
const PASS: &str = "update virtual shadow atlas";
const QUAD: &str = "shadow tile corners";
const INDICES: &str = "shadow tile indices";
const UPLOAD_QUAD: &str = "upload shadow tile corners";
const UPLOAD_INDICES: &str = "upload shadow tile indices";

#[allow(unsafe_code)]
fn clear_program() -> ManagedRasterProgram {
    static DESC: RasterProgramDescriptor = RasterProgramDescriptor {
        entry: "main",
        vertex_stride: 8,
        secondary_vertex_stride: 0,
        vertex_attributes: &[VertexAttribute {
            stream: 0,
            location: 0,
            offset: 0,
            format: VertexFormat::Float32x2,
        }],
        parameters: RasterParameterLayout {
            size: 0,
            stages: ShaderStages::EMPTY,
        },
        color_outputs: &[],
        bindings: &[BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::StorageRead,
            stages: ShaderStages::VERTEX,
            min_size: SLOT_BYTES as u64,
        }],
        binding_bytes: &[SLOT_BYTES as u64],
        sampling_pairs: &[],
        blend: BlendState::Replace,
        depth: DepthState::Required {
            compare: CompareOp::Always,
            write: true,
            require_load: true,
        },
        shaders: sashiko_renderer_core::program::embedded_shader_binaries!(
            sashiko_renderer_core::program::RasterShaderStages {
                vertex: include_bytes!("../../../shaders/virtual_shadow/clear_tile.vert.spv"),
                fragment: None
            },
            sashiko_renderer_core::program::D3D12RasterShaders {
                stages: sashiko_renderer_core::program::RasterShaderStages {
                    vertex: include_bytes!(
                        "../../../shaders/virtual_shadow/dxil/clear_tile.vert.dxil"
                    ),
                    fragment: None
                },
                inline_constants: None
            },
        ),
    };
    // SAFETY: one float2 vertex input, one read-only 16-byte record for an
    // instance id clamped to 1023. The vertex stage emits fixed-function depth.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&DESC) }
}

/// Reserve one shared atlas for a view and the per-frame dirty-tile clear.
/// Reserve at most one caster draw per clip level for each input mesh range.
pub fn reserve_atlas(preparation: &mut Preparation, max_casters: usize) -> Result<(), GraphError> {
    let max_draws = max_casters
        .checked_mul(super::LEVELS)
        .ok_or_else(|| GraphError::invalid_input("virtual shadow caster count overflow"))?;
    use sashiko_renderer_core::capacity::{ResourceClass, WorkingSet};
    preparation.add_working_set(WorkingSet {
        resources: &[ResourceClass::Image {
            desc: ATLAS_DESC,
            count: 1,
        }],
        ..Default::default()
    })?;
    preparation.reserve_buffer(BufferDesc { size: 32 }, 1)?;
    preparation.reserve_buffer(BufferDesc { size: 24 }, 1)?;
    preparation.add_graph_requirements(GraphRequirements {
        resources: 3,
        initial_ranges: 1,
        passes: 2,
        accesses: 2,
        upload_bytes: 56,
        name_bytes: QUAD.len()
            + INDICES.len()
            + UPLOAD_QUAD.len()
            + UPLOAD_INDICES.len()
            + PASS.len(),
        ..Default::default()
    })?;
    preparation.reserve_raster_pass(PASS, SampleCount::X1, |pass| {
        pass.attachment()?;
        pass.draws(&clear_program(), 1)?;
        if max_draws > 0 {
            pass.draws(&super::caster::program(), max_draws)?;
        }
        Ok(())
    })
}

/// Load the previous atlas when its contents are available. Clear only the
/// dirty slots, then record their casters in the same raster pass. A fresh
/// atlas uses a full attachment clear to establish valid depth everywhere.
pub fn draw_atlas(
    graph: &mut Graph,
    atlas: ImageHandle,
    pages: &AssignedPages,
    preserve: bool,
    casters: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
) -> Result<(), GraphError> {
    if graph.image_desc(atlas)? != ATLAS_DESC {
        return Err(GraphError::invalid_input(
            "virtual shadow atlas must be 4096x4096 D32",
        ));
    }
    let program = clear_program();
    let clear = if preserve {
        let vertices = graph.buffer(QUAD, BufferDesc { size: 32 })?;
        let indices = graph.buffer(INDICES, BufferDesc { size: 24 })?;
        let corners = [0.0_f32, 0., 1., 0., 0., 1., 1., 1.].map(f32::to_le_bytes);
        graph.upload_buffer(UPLOAD_QUAD, vertices, 0, corners.as_flattened())?;
        let faces = [0u32, 1, 2, 2, 1, 3].map(u32::to_le_bytes);
        graph.upload_buffer(UPLOAD_INDICES, indices, 0, faces.as_flattened())?;
        let binding = Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::Buffer {
                buffer: pages.slots,
                offset: 0,
                size: SLOT_BYTES as u64,
            },
        };
        Some(RasterDraw {
            bindings: Some(graph.raster_bindings(&program, &[binding])?),
            secondary_vertices: None,
            vertices,
            indices,
            first_index: 0,
            index_count: 6,
            instance_count: PHYSICAL_PAGES as u32,
            parameters: RasterConstants::default(),
        })
    } else {
        None
    };
    let view = graph.image_view(atlas, 0, 0)?;
    graph.raster(
        PASS,
        RasterAttachments {
            colors: &[],
            color_resolves: &[],
            depth: Some(Attachment {
                view,
                load: if preserve {
                    LoadOp::Load
                } else {
                    LoadOp::Clear(1.)
                },
                store: StoreOp::Store,
            }),
        },
        |pass| {
            if let Some(draw) = clear {
                pass.draw(&program, [draw])?;
            }
            casters(pass)
        },
    )?;
    Ok(())
}

use crate::{
    passes::pbr::SHADOW,
    pbr::{AlphaMode, GraphMaterial, Transform, ViewInput},
    pipeline::Mesh,
};
use sashiko_renderer_core::{
    capacity::{GraphRequirements, Preparation},
    graph::{
        Attachment, Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError,
        ImageDesc, ImageView, LoadOp, RasterAttachments, RasterConstants, RasterDraw, SampleCount,
        StoreOp,
    },
    program::{
        BindingLayout, ManagedRasterProgram, RasterParameterLayout, RasterProgramDescriptor,
        ShaderStages,
    },
};

const PASS: &str = "virtual shadow receivers";
const IMAGE: &str = "virtual shadow receiver depth";
const CAMERA: &str = "virtual shadow receiver projection";

#[allow(unsafe_code)]
fn program() -> ManagedRasterProgram {
    static DESC: RasterProgramDescriptor = RasterProgramDescriptor {
        parameters: RasterParameterLayout {
            size: 52,
            stages: ShaderStages::VERTEX,
        },
        bindings: &[
            BindingLayout {
                min_size: 64,
                ..SHADOW.bindings[0]
            },
            SHADOW.bindings[1],
            SHADOW.bindings[2],
            SHADOW.bindings[3],
        ],
        binding_bytes: &[64, 64, 0, 0],
        shaders: sashiko_renderer_core::program::embedded_shader_binaries!(
            sashiko_renderer_core::program::RasterShaderStages {
                vertex: include_bytes!("../../../shaders/virtual_shadow/camera_depth.vert.spv"),
                fragment: Some(include_bytes!(
                    "../../../shaders/shadow_transformed.frag.spv"
                )),
            },
            sashiko_renderer_core::program::D3D12RasterShaders {
                stages: sashiko_renderer_core::program::RasterShaderStages {
                    vertex: include_bytes!(
                        "../../../shaders/virtual_shadow/dxil/camera_depth.vert.dxil"
                    ),
                    fragment: Some(include_bytes!(
                        "../../../shaders/dxil/shadow_transformed.frag.dxil"
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
    // SAFETY: fixed-index loads of one 4x4 matrix and 13 inline words.
    // The standard masked shadow fragment uses the same 48-byte mesh ABI,
    // material UBO, base-color texture, sampler, UV and orientation parity.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&DESC) }
}

pub fn reserve_camera_depth(
    prep: &mut Preparation,
    extent: [u32; 2],
    max_draws: usize,
) -> Result<(), GraphError> {
    prep.reserve_buffer(BufferDesc { size: 64 }, 1)?;
    prep.add_graph_requirements(GraphRequirements {
        resources: 1,
        passes: 1,
        accesses: 1,
        upload_bytes: 64,
        name_bytes: 2 * CAMERA.len(),
        ..Default::default()
    })?;
    prep.reserve_raster_pass(PASS, SampleCount::X1, |pass| {
        pass.transient_attachment(IMAGE, ImageDesc::depth32(extent[0], extent[1]))?;
        pass.draws(&program(), max_draws)
    })
}

pub fn camera_matrix(graph: &mut Graph, view: ViewInput) -> Result<BufferHandle, GraphError> {
    let matrix = graph.buffer(CAMERA, BufferDesc { size: 64 })?;
    let bytes = view.clip_from_world().map(|col| col.map(f32::to_le_bytes));
    graph.upload_buffer(CAMERA, matrix, 0, bytes.as_flattened().as_flattened())?;
    Ok(matrix)
}

pub fn bind_receiver(
    graph: &mut Graph,
    camera: BufferHandle,
    mesh: Mesh,
    transform: Transform,
    material: GraphMaterial,
) -> Result<Option<RasterDraw>, GraphError> {
    if material.alpha_mode() == AlphaMode::Blend {
        return Ok(None);
    }
    if mesh.vertex_stride != SHADOW.vertex_stride {
        return Err(GraphError::invalid_input(
            "virtual shadow receivers require the PBR vertex stream",
        ));
    }
    let bindings = material.factors().raster_bindings(
        graph,
        &program(),
        &[
            Binding {
                group: 0,
                binding: 0,
                resource: BindingResource::Buffer {
                    buffer: camera,
                    offset: 0,
                    size: 64,
                },
            },
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
    let mut words = [0; 13];
    words[..12].copy_from_slice(transform.model_key().as_flattened());
    words[12] = u32::from(transform.mirrored());
    Ok(Some(RasterDraw {
        vertices: mesh.vertices,
        indices: mesh.indices,
        first_index: mesh.first_index,
        index_count: mesh.index_count,
        instance_count: 1,
        secondary_vertices: None,
        bindings: Some(bindings),
        parameters: RasterConstants::new(&words)?,
    }))
}

/// Render current opaque and masked receivers at the camera's native extent.
pub fn draw_camera_depth(
    graph: &mut Graph,
    extent: [u32; 2],
    draws: &[RasterDraw],
) -> Result<ImageView, GraphError> {
    let image = graph.image(IMAGE, ImageDesc::depth32(extent[0], extent[1]))?;
    let view = graph.image_view(image, 0, 0)?;
    graph.raster(
        PASS,
        RasterAttachments {
            colors: &[],
            color_resolves: &[],
            depth: Some(Attachment {
                view,
                load: LoadOp::Clear(1.),
                store: StoreOp::Store,
            }),
        },
        |pass| {
            if !draws.is_empty() {
                pass.draw(&program(), draws.iter().copied())?;
            }
            Ok(())
        },
    )?;
    Ok(view)
}

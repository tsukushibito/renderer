//! Image layers in display-linear space. The caller owns layout and hit testing.
use renderer_core::{
    ImageFormat,
    graph::{
        Attachment, Binding, BindingResource, Graph, GraphError, ImageHandle, ImageView, LoadOp,
        RasterAttachments, RasterConstants, RasterRect, StoreOp,
    },
    program::{
        BindingLayout, BindingType, BlendState, D3D12InlineConstantBinding, DepthState,
        ManagedRasterProgram, RasterParameterLayout, RasterProgramDescriptor,
        SampledImageSamplerPair, SamplerDescriptor, ShaderStages, fullscreen_triangle_shaders,
    },
};

/// A premultiplied-alpha image with an optional clip in target coordinates.
/// A transparent canvas output can be reused directly as another layer.
pub struct ImageLayer {
    pub source: ImageView,
    pub rect: RasterRect,
    pub clip: Option<RasterRect>,
}

pub fn program() -> ManagedRasterProgram {
    static DESCRIPTOR: RasterProgramDescriptor = RasterProgramDescriptor {
        entry: "main",
        vertex_stride: 0,
        vertex_attributes: &[],
        parameters: RasterParameterLayout {
            size: 16,
            stages: ShaderStages::FRAGMENT,
        },
        color_outputs: &[ImageFormat::Rgba8Unorm],
        bindings: &[
            BindingLayout {
                group: 0,
                binding: 0,
                kind: BindingType::SampledImage,
                min_size: 0,
                stages: ShaderStages::FRAGMENT,
            },
            BindingLayout {
                group: 0,
                binding: 1,
                kind: BindingType::Sampler,
                min_size: 0,
                stages: ShaderStages::FRAGMENT,
            },
        ],
        binding_bytes: &[0, 0],
        sampling_pairs: &[SampledImageSamplerPair {
            image_group: 0,
            image_binding: 0,
            sampler_group: 0,
            sampler_binding: 1,
        }],
        blend: BlendState::PremultipliedAlpha,
        depth: DepthState::Disabled,
        shaders: fullscreen_triangle_shaders(
            renderer_core::program::embedded_shader_binaries!(
                include_bytes!("../shaders/canvas.spv"),
                include_bytes!("../shaders/canvas.dxil")
            ),
            Some(D3D12InlineConstantBinding {
                register: 0,
                space: 0,
            }),
        ),
    };
    // SAFETY: canvas.hlsl samples one texture/sampler at LOD zero. Four f32
    // constants specify a nonempty viewport. It writes only color attachment 0.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&DESCRIPTOR) }
}

/// Borrow the host draw list for this recording. A clear starts a UI-only
/// target; loading preserves earlier scene rendering.
pub fn record(
    graph: &mut Graph,
    target: ImageHandle,
    load: LoadOp<[f32; 4]>,
    layers: &[ImageLayer],
) -> Result<(), GraphError> {
    graph.raster(
        "canvas",
        RasterAttachments {
            colors: &[Attachment {
                view: graph.image_view(target, 0, 0)?,
                load,
                store: StoreOp::Store,
            }],
            color_resolves: &[],
            depth: None,
        },
        |pass| {
            for layer in layers {
                let rect = layer.rect;
                pass.set_viewport(Some(rect))?;
                pass.set_scissor(layer.clip)?;
                let words = [rect.x, rect.y, rect.width, rect.height].map(|v| (v as f32).to_bits());
                pass.draw_fullscreen(
                    &program(),
                    &[
                        Binding {
                            group: 0,
                            binding: 0,
                            resource: BindingResource::ImageView(layer.source),
                        },
                        Binding {
                            group: 0,
                            binding: 1,
                            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                        },
                    ],
                    RasterConstants::new(&words)?,
                )?;
            }
            Ok(())
        },
    )?;
    Ok(())
}

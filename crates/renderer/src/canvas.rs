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
#[derive(Clone, Copy)]
pub enum Paint {
    Image(ImageView),
    /// Premultiplied linear RGBA.
    Color([f32; 4]),
}

#[derive(Clone, Copy)]
pub struct Layer {
    pub paint: Paint,
    pub rect: RasterRect,
    pub clip: Option<RasterRect>,
}

/// Raster program used when including a Canvas in a host-owned working set.
#[allow(unsafe_code)]
fn program(format: ImageFormat, solid: bool) -> Result<ManagedRasterProgram, GraphError> {
    static DESCRIPTOR: RasterProgramDescriptor = RasterProgramDescriptor {
        entry: "main",
        vertex_stride: 0,
        secondary_vertex_stride: 0,
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
                include_bytes!("../shaders/canvas.frag.spv"),
                include_bytes!("../shaders/dxil/canvas.frag.dxil")
            ),
            Some(D3D12InlineConstantBinding {
                register: 0,
                space: 0,
            }),
        ),
    };
    static FLOAT: RasterProgramDescriptor = RasterProgramDescriptor {
        color_outputs: &[ImageFormat::Rgba16Float],
        ..DESCRIPTOR
    };
    static FILL: RasterProgramDescriptor = RasterProgramDescriptor {
        bindings: &[],
        binding_bytes: &[],
        sampling_pairs: &[],
        shaders: fullscreen_triangle_shaders(
            renderer_core::program::embedded_shader_binaries!(
                include_bytes!("../shaders/canvas_fill.frag.spv"),
                include_bytes!("../shaders/dxil/canvas_fill.frag.dxil")
            ),
            Some(D3D12InlineConstantBinding {
                register: 0,
                space: 0,
            }),
        ),
        ..DESCRIPTOR
    };
    static FLOAT_FILL: RasterProgramDescriptor = RasterProgramDescriptor {
        color_outputs: &[ImageFormat::Rgba16Float],
        ..FILL
    };
    let descriptor = match (format, solid) {
        (ImageFormat::Rgba8Unorm, false) => &DESCRIPTOR,
        (ImageFormat::Rgba8Unorm, true) => &FILL,
        (ImageFormat::Rgba16Float, false) => &FLOAT,
        (ImageFormat::Rgba16Float, true) => &FLOAT_FILL,
        _ => {
            return Err(GraphError::invalid_input(
                "canvas requires a linear RGBA8 or RGBA16F target",
            ));
        }
    };
    // SAFETY: canvas.frag.hlsl samples one texture/sampler at LOD zero. Four f32
    // constants specify a nonempty viewport or the fill shader constant RGBA. It writes only color attachment 0.
    Ok(unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) })
}

/// Sampler for Canvas image draws. Reserve it when a host supports image layers.
pub const IMAGE_SAMPLER: SamplerDescriptor = SamplerDescriptor::NearestClamp;

/// Raster programs for images and solid rectangles, for custom host preparation.
/// Reserve [`IMAGE_SAMPLER`] alongside the image program.
pub fn programs(format: ImageFormat) -> Result<[ManagedRasterProgram; 2], GraphError> {
    Ok([program(format, false)?, program(format, true)?])
}

/// Borrow the host draw list for this recording. A clear starts a UI-only
/// target; loading preserves earlier scene rendering.
pub fn record(
    graph: &mut Graph,
    target: ImageHandle,
    load: LoadOp<[f32; 4]>,
    layers: &[Layer],
) -> Result<(), GraphError> {
    let [image, fill] = programs(graph.image_desc(target)?.format)?;
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
                let source = match layer.paint {
                    Paint::Image(view) => view,
                    Paint::Color(rgba) => {
                        pass.draw_fullscreen(
                            &fill,
                            &[],
                            RasterConstants::new(&rgba.map(f32::to_bits))?,
                        )?;
                        continue;
                    }
                };
                let words = [rect.x, rect.y, rect.width, rect.height].map(|v| (v as f32).to_bits());
                pass.draw_fullscreen(
                    &image,
                    &[
                        Binding {
                            group: 0,
                            binding: 0,
                            resource: BindingResource::ImageView(source),
                        },
                        Binding {
                            group: 0,
                            binding: 1,
                            resource: BindingResource::Sampler(IMAGE_SAMPLER),
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

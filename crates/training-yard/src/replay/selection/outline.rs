//! The host owns the selected actor and the appearance of its outline.
use renderer::{
    core::{ImageFormat, graph::*, program::*, runtime::FrameCompletion},
    picking::PickImage,
    pipeline::{PipelineResult, PreparedSetup},
};

#[allow(unsafe_code)]
fn program() -> ManagedRasterProgram {
    static PROGRAM: RasterProgramDescriptor = RasterProgramDescriptor {
        entry: "main",
        vertex_stride: 0,
        vertex_attributes: &[],
        parameters: RasterParameterLayout {
            size: 4,
            stages: ShaderStages::FRAGMENT,
        },
        color_outputs: &[ImageFormat::Rgba8Unorm],
        bindings: &[BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        }],
        binding_bytes: &[0],
        sampling_pairs: &[],
        blend: BlendState::PremultipliedAlpha,
        depth: DepthState::Disabled,
        shaders: fullscreen_triangle_shaders(
            renderer::core::program::embedded_shader_binaries!(
                include_bytes!("../../../../renderer/shaders/selection_outline.frag.spv"),
                include_bytes!("../../../../renderer/shaders/dxil/selection_outline.frag.dxil")
            ),
            Some(D3D12InlineConstantBinding {
                register: 0,
                space: 0,
            }),
        ),
    };
    // SAFETY: the pinned fragment shader bounds-checks loads from its single
    // sampled texture, reads one inline u32 and writes premultiplied RGBA.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&PROGRAM) }
}

pub(super) fn prepare(plan: &mut PreparedSetup<'_>, [w, h]: [u32; 2]) -> PipelineResult<()> {
    plan.preparation_mut().reserve_raster_pass(
        "selected armor outline",
        SampleCount::X1,
        |pass| {
            pass.attachment()?;
            pass.draws(&program(), 1)
        },
    )?;
    for name in ["selection map", "outlined viewport"] {
        plan.reserve_readback(name, ImageDesc::rgba8(w, h))?;
    }
    Ok(())
}

pub(crate) struct Readbacks {
    ids: PassHandle,
    outlined: PassHandle,
    selected: Option<u32>,
}
pub(super) fn record(
    graph: &mut Graph,
    target: ImageHandle,
    image: PickImage,
    selected: Option<u32>,
) -> PipelineResult<Readbacks> {
    if let Some(id) = selected {
        let view = graph.image_view(image.image(), 0, 0)?;
        graph.raster(
            "selected armor outline",
            RasterAttachments {
                colors: &[Attachment {
                    view: graph.image_view(target, 0, 0)?,
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                }],
                color_resolves: &[],
                depth: None,
            },
            |pass| {
                pass.draw_fullscreen(
                    &program(),
                    &[Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::ImageView(view),
                    }],
                    RasterConstants::new(&[id])?,
                )
            },
        )?;
    }
    Ok(Readbacks {
        ids: graph.readback_image("selection map", image.image())?,
        outlined: graph.readback_image("outlined viewport", target)?,
        selected,
    })
}

impl Readbacks {
    pub fn verify(
        &self,
        result: &mut FrameCompletion,
        base: &[u8],
        output: &mut [u8; 48 * 24 * 4],
    ) -> PipelineResult<()> {
        let mut ids = [0; 48 * 24 * 4];
        result.readback_into(self.ids, &mut ids)?;
        result.readback_into(self.outlined, output)?;
        let selected = |x: i32, y: i32| {
            (0..48).contains(&x)
                && (0..24).contains(&y)
                && self.selected
                    == Some(u32::from_le_bytes(
                        ids.as_chunks::<4>().0[y as usize * 48 + x as usize],
                    ))
        };
        let mut edges = 0;
        for (i, pixel) in output.as_chunks::<4>().0.iter().enumerate() {
            let (x, y) = ((i % 48) as i32, (i / 48) as i32);
            let border = !selected(x, y)
                && [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]
                    .into_iter()
                    .any(|(x, y)| selected(x, y));
            if border {
                edges += 1;
            }
            let expected = if border {
                &[255, 0, 255, 255]
            } else {
                &base.as_chunks::<4>().0[i]
            };
            assert_eq!(pixel, expected, "outline pixel ({x},{y})");
        }
        assert_eq!(edges > 0, self.selected.is_some());
        Ok(())
    }
}

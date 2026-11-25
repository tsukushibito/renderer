//! Immutable pass layouts and WebGPU graphics pipeline variants.
use super::super::{
    bindings::{ImmediateBinding, ProgramLayout},
    engine::format,
};
use crate::{
    graph::SampleCount,
    program::{BlendState, CompareOp, ManagedRasterProgram, RasterPipelineRequest, VertexFormat},
    runtime::{ExecutionError, ExecutionErrorKind},
};

/// The pipeline must declare the pass's depth format even when testing is disabled.
pub(super) struct Pipeline {
    pub program: ManagedRasterProgram,
    pub samples: SampleCount,
    pub layout: ProgramLayout,
    with_depth: wgpu::RenderPipeline,
    color_only: Option<wgpu::RenderPipeline>,
}

impl Pipeline {
    pub fn new(
        device: &wgpu::Device,
        request: &RasterPipelineRequest,
    ) -> Result<Self, ExecutionError> {
        let program = &request.program;
        let desc = program.descriptor();
        let shaders = program.wgsl().ok_or_else(|| {
            ExecutionError::webgpu(
                ExecutionErrorKind::Unsupported,
                "raster program requires a WebGPU shader",
            )
        })?;
        let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(shaders.vertex.into()),
        });
        let fragment = shaders.fragment.map(|source| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: None,
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        });

        let immediate = (desc.parameters.size != 0).then_some(ImmediateBinding {
            size: u64::from(desc.parameters.size.next_multiple_of(16)),
            stages: desc.parameters.stages,
        });
        let layout = ProgramLayout::new(
            device,
            desc.bindings,
            desc.sampling_pairs,
            program.webgpu_nonfiltering_samplers(),
            immediate,
        )?;
        let mut attributes = [Vec::new(), Vec::new()];
        for attr in desc.vertex_attributes {
            attributes[attr.stream as usize].push(wgpu::VertexAttribute {
                shader_location: attr.location,
                offset: attr.offset.into(),
                format: match attr.format {
                    VertexFormat::Float32x2 => wgpu::VertexFormat::Float32x2,
                    VertexFormat::Float32x3 => wgpu::VertexFormat::Float32x3,
                    VertexFormat::Float32x4 => wgpu::VertexFormat::Float32x4,
                },
            });
        }
        let streams: Vec<_> = [desc.vertex_stride, desc.secondary_vertex_stride]
            .into_iter()
            .zip(&attributes)
            .filter(|(stride, _)| *stride != 0)
            .map(|(stride, attributes)| {
                Some(wgpu::VertexBufferLayout {
                    array_stride: stride.into(),
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes,
                })
            })
            .collect();
        let targets: Vec<_> = desc
            .color_outputs
            .iter()
            .map(|&image_format| {
                Some(wgpu::ColorTargetState {
                    format: format(image_format),
                    blend: blend(desc.blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })
            })
            .collect();
        let make = |depth: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: Some(layout.pipeline()),
                vertex: wgpu::VertexState {
                    module: &vertex,
                    entry_point: Some(desc.entry),
                    compilation_options: Default::default(),
                    buffers: &streams,
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: depth.then(|| {
                    depth_state(
                        desc.depth.compare().unwrap_or(CompareOp::Always),
                        desc.depth.write_enabled(),
                    )
                }),
                multisample: wgpu::MultisampleState {
                    count: request.sample_count.as_u32(),
                    ..Default::default()
                },
                fragment: fragment.as_ref().map(|module| wgpu::FragmentState {
                    module,
                    entry_point: Some(desc.entry),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let with_depth = make(true);
        let color_only = (!program.requires_depth()).then(|| make(false));
        Ok(Self {
            program: program.clone(),
            samples: request.sample_count,
            with_depth,
            color_only,
            layout,
        })
    }

    pub fn select(&self, depth: bool) -> Result<&wgpu::RenderPipeline, ExecutionError> {
        if depth {
            Ok(&self.with_depth)
        } else {
            self.color_only.as_ref().ok_or_else(|| {
                ExecutionError::invalid("raster program requires a depth attachment")
            })
        }
    }
}

fn blend(mode: BlendState) -> Option<wgpu::BlendState> {
    match mode {
        BlendState::Replace => None,
        BlendState::Alpha => Some(wgpu::BlendState::ALPHA_BLENDING),
        BlendState::PremultipliedAlpha => Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        BlendState::Additive => {
            let add = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            };
            Some(wgpu::BlendState {
                color: add,
                alpha: add,
            })
        }
    }
}

fn depth_state(compare: CompareOp, write: bool) -> wgpu::DepthStencilState {
    let compare = match compare {
        CompareOp::Never => wgpu::CompareFunction::Never,
        CompareOp::Less => wgpu::CompareFunction::Less,
        CompareOp::Equal => wgpu::CompareFunction::Equal,
        CompareOp::LessEqual => wgpu::CompareFunction::LessEqual,
        CompareOp::Greater => wgpu::CompareFunction::Greater,
        CompareOp::NotEqual => wgpu::CompareFunction::NotEqual,
        CompareOp::GreaterEqual => wgpu::CompareFunction::GreaterEqual,
        CompareOp::Always => wgpu::CompareFunction::Always,
    };
    wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(write),
        depth_compare: Some(compare),
        stencil: Default::default(),
        bias: Default::default(),
    }
}

/// Graph::clear_depth_rect always restores far depth. Scissoring this triangle
/// preserves the surrounding shadow atlas and does not require a CPU upload.
pub(super) fn clear_pipeline(device: &wgpu::Device, samples: SampleCount) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("clear depth rectangle"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
            @vertex fn main(@builtin(vertex_index) id: u32) -> @builtin(position) vec4<f32> {
                let p = array(vec2(-1.0,-1.0), vec2(3.0,-1.0), vec2(-1.0,3.0));
                return vec4(p[id], 1.0, 1.0);
            }
        "#
            .into(),
        ),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("clear depth rectangle"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: Some(depth_state(CompareOp::Always, true)),
        multisample: wgpu::MultisampleState {
            count: samples.as_u32(),
            ..Default::default()
        },
        fragment: None,
        multiview_mask: None,
        cache: None,
    })
}

//! Translate the ordered raster batches of a compiled graph into WebGPU render passes.
mod pipeline;

use self::pipeline::{Pipeline, clear_pipeline};
use super::{bindings::BindGroups, engine::Resources};
use crate::{
    capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind},
    graph::{
        AttachmentOperand, CompiledGraph, LoadAction, Operation, ResourceDesc, SampleCount, StoreOp,
    },
    program::{INDIRECT_COMMAND_BYTES, MAX_RASTER_COLOR_ATTACHMENTS, RasterPipelineRequest},
    runtime::{ExecutionError, ExecutionErrorKind, RuntimeLimits, WorkingSet},
};

pub(super) struct Raster {
    pipelines: Vec<Pipeline>,
    groups: BindGroups,
    constants: DrawConstants,
    clear: [wgpu::RenderPipeline; 2],
}

impl Raster {
    pub(super) fn new(
        device: &wgpu::Device,
        limits: RuntimeLimits,
        set: WorkingSet<'_>,
    ) -> Result<Self, ExecutionError> {
        // A group for inline constants may be needed even if there are no Graph bindings.
        let groups = limits
            .graph
            .draws
            .checked_mul(4)
            .ok_or_else(|| ExecutionError::invalid("WebGPU raster group capacity overflow"))?;
        let mut raster = Self {
            pipelines: Vec::new(),
            groups: BindGroups::new(groups)?,
            constants: DrawConstants::new(device, limits)?,
            clear: [
                clear_pipeline(device, SampleCount::X1),
                clear_pipeline(device, SampleCount::X4),
            ],
        };
        raster.prepare(device, set.raster)?;
        Ok(raster)
    }

    /// Replace the complete set only after building every requested variant.
    pub(super) fn prepare(
        &mut self,
        device: &wgpu::Device,
        requests: &[RasterPipelineRequest],
    ) -> Result<(), ExecutionError> {
        let pipelines = requests
            .iter()
            .map(|request| Pipeline::new(device, request))
            .collect::<Result<_, _>>()?;
        self.pipelines = pipelines;
        self.clear_bindings();
        Ok(())
    }

    pub(super) fn clear_bindings(&mut self) {
        self.groups.clear();
    }

    pub(super) fn begin(&mut self) {
        self.constants.used = 0;
    }

    /// Enqueue before submitting this graph's commands. The queue orders these
    /// writes after previous submits, even while a previous graph is still in flight.
    pub(super) fn flush_constants(&self, queue: &wgpu::Queue) {
        if self.constants.used != 0 {
            queue.write_buffer(
                &self.constants.buffer,
                0,
                &self.constants.bytes[..self.constants.used],
            );
        }
    }

    pub(super) fn record(
        &mut self,
        device: &wgpu::Device,
        resources: &Resources<'_>,
        graph: &CompiledGraph,
        pass: usize,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), ExecutionError> {
        let graph = graph.data();
        let (colors, depth, batches) = match &graph.passes[pass].operation {
            Operation::Raster {
                colors,
                depth,
                batches,
                ..
            } => (colors, *depth, graph.raster_batches(batches)),
            Operation::AttachmentPass { colors, depth } => (colors, *depth, &[][..]),
            _ => {
                return Err(ExecutionError::invalid(
                    "expected raster or attachment pass",
                ));
            }
        };
        let colors = graph.raster_attachments(colors);
        let depth = depth.map(|index| graph.raster_attachment(index));
        let first = depth
            .or_else(|| colors.first())
            .expect("Graph attachment pass has a target");
        let ResourceDesc::Image(desc) = graph.resources[first.view.image().index].desc else {
            unreachable!()
        };
        let [width, height] = desc
            .mip_extent(first.view.mip_level())
            .expect("existing image view mip");
        let mut attachments = std::array::from_fn::<_, MAX_RASTER_COLOR_ATTACHMENTS, _>(|_| None);
        for (slot, a) in attachments.iter_mut().zip(colors) {
            *slot = Some(wgpu::RenderPassColorAttachment {
                view: resources.view(a.view)?,
                depth_slice: None,
                resolve_target: a.resolve.map(|v| resources.view(v)).transpose()?,
                ops: color_ops(a),
            });
        }
        let depth_attachment = depth
            .map(|a| {
                Ok::<_, ExecutionError>(wgpu::RenderPassDepthStencilAttachment {
                    view: resources.view(a.view)?,
                    depth_ops: Some(wgpu::Operations {
                        load: load(a.ops.load, a.clear[0]),
                        store: store(a.ops.store),
                    }),
                    stencil_ops: None,
                })
            })
            .transpose()?;
        let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &attachments[..colors.len()],
            depth_stencil_attachment: depth_attachment,
            ..Default::default()
        });
        if let Some(rect) = depth.and_then(|a| a.depth_clear_rect) {
            render.set_pipeline(&self.clear[usize::from(desc.sample_count == SampleCount::X4)]);
            render.set_scissor_rect(rect.x, rect.y, rect.width, rect.height);
            render.draw(0..3, 0..1);
        }
        for batch in batches {
            let pipeline = self
                .pipelines
                .iter()
                .find(|p| p.program.identity() == batch.program && p.samples == desc.sample_count)
                .ok_or_else(|| {
                    ExecutionError::webgpu(
                        ExecutionErrorKind::Unsupported,
                        "raster program/sample count absent from the prepared WebGPU working set",
                    )
                })?;
            render.set_pipeline(pipeline.select(depth.is_some())?);
            let (viewport, scissor) = batch.regions.resolve(width, height);
            render.set_viewport(
                viewport.x as f32,
                viewport.y as f32,
                viewport.width as f32,
                viewport.height as f32,
                0.,
                1.,
            );
            render.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
            for (draw, words) in graph
                .raster_draws(&batch.draws)
                .iter()
                .zip(graph.raster_constants(batch))
            {
                let offset = self.constants.write(words)?;
                let bindings = self.groups.resolve(
                    device,
                    &pipeline.layout,
                    graph.raster_bindings(&draw.bindings),
                    resources,
                    offset.map(|_| &self.constants.buffer),
                )?;
                for (index, group) in bindings.iter().enumerate() {
                    if let Some(group) = group {
                        let offsets = if index == 0 { offset.as_slice() } else { &[] };
                        render.set_bind_group(index as u32, group, offsets);
                    }
                }
                if let Some(mesh) = draw.indexed {
                    render.set_vertex_buffer(0, resources.buffer(mesh.vertices)?.slice(..));
                    if let Some(second) = mesh.secondary_vertices {
                        render.set_vertex_buffer(1, resources.buffer(second)?.slice(..));
                    }
                    render.set_index_buffer(
                        resources.buffer(mesh.indices)?.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    if let Some(indirect) = batch.indirect {
                        let arguments = resources.buffer(indirect.buffer)?;
                        for command in 0..u64::from(indirect.command_count) {
                            render.draw_indexed_indirect(
                                arguments,
                                indirect.command_offset + command * INDIRECT_COMMAND_BYTES,
                            );
                        }
                    } else {
                        let end =
                            mesh.first_index
                                .checked_add(mesh.index_count)
                                .ok_or_else(|| {
                                    ExecutionError::invalid("WebGPU index range overflows")
                                })?;
                        render.draw_indexed(mesh.first_index..end, 0, 0..mesh.instance_count);
                    }
                } else {
                    render.draw(0..3, 0..1);
                }
            }
        }
        Ok(())
    }
}

fn load<T>(action: LoadAction, clear: T) -> wgpu::LoadOp<T> {
    match action {
        LoadAction::Load => wgpu::LoadOp::Load,
        // A discard allows any starting value. Clearing supplies defined initial
        // texels under WebGPU's memory model and does not change Graph validity.
        LoadAction::Clear | LoadAction::Discard => wgpu::LoadOp::Clear(clear),
    }
}

fn store(op: StoreOp) -> wgpu::StoreOp {
    match op {
        StoreOp::Store => wgpu::StoreOp::Store,
        StoreOp::Discard => wgpu::StoreOp::Discard,
    }
}

fn color_ops(a: &AttachmentOperand) -> wgpu::Operations<wgpu::Color> {
    let [r, g, b, alpha] = a.clear.map(f64::from);
    wgpu::Operations {
        load: load(a.ops.load, wgpu::Color { r, g, b, a: alpha }),
        store: store(a.ops.store),
    }
}

struct DrawConstants {
    buffer: wgpu::Buffer,
    bytes: Vec<u8>,
    stride: usize,
    used: usize,
}

impl DrawConstants {
    fn new(device: &wgpu::Device, limits: RuntimeLimits) -> Result<Self, ExecutionError> {
        let stride =
            128usize.next_multiple_of(device.limits().min_uniform_buffer_offset_alignment as usize);
        let slots = limits
            .graph
            .raster_constant_words
            .unwrap_or(limits.graph.draws)
            .min(limits.graph.draws);
        let size = slots
            .max(1)
            .checked_mul(stride)
            .filter(|&n| n as u64 <= device.limits().max_buffer_size && n <= u32::MAX as usize)
            .ok_or_else(|| {
                ExecutionError::webgpu(
                    ExecutionErrorKind::Unsupported,
                    "draw constant arena exceeds WebGPU buffer/offset limits",
                )
            })?;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raster draw constants"),
            size: size as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(|_| {
            ExecutionError::webgpu(
                ExecutionErrorKind::CapacityExhausted,
                "cannot reserve WebGPU draw constant staging",
            )
        })?;
        bytes.resize(size, 0);
        Ok(Self {
            buffer,
            bytes,
            stride,
            used: 0,
        })
    }

    fn write(&mut self, words: &[u32]) -> Result<Option<u32>, ExecutionError> {
        if words.is_empty() {
            return Ok(None);
        }
        let offset = self
            .used
            .checked_next_multiple_of(self.stride)
            .ok_or_else(|| ExecutionError::invalid("WebGPU constant offset overflows"))?;
        let end = offset
            .checked_add(std::mem::size_of_val(words))
            .ok_or_else(|| ExecutionError::invalid("WebGPU constant range overflows"))?;
        let cap = self.bytes.len();
        let dst = self.bytes.get_mut(offset..end).ok_or_else(|| {
            ExecutionError::with_capacity(
                "raster draw constant capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::Reconfigure,
                    Some(cap as u64),
                    Some(end as u64),
                ),
            )
        })?;
        for (word, bytes) in words.iter().zip(dst.as_chunks_mut::<4>().0) {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
        self.used = end;
        Ok(Some(offset as u32))
    }
}

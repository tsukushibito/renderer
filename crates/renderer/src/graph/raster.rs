//! Ordered graphics commands recorded into one attachment scope.

use super::builder::read;
use super::storage::capacity_error;
use super::{
    AccessPurpose, BufferHandle, Graph, GraphCapacityKind, GraphError, GraphErrorKind, LoadOp,
    Operation, PassHandle, RasterAttachments, RasterBatchOperands, RasterDraw, RasterDrawOperands,
};
use crate::program::ManagedRasterProgram;
use std::borrow::Borrow;

/// A temporary recorder for ordered draw batches in one Raster attachment scope.
/// On error the enclosing pass is discarded, including any earlier batches.
pub struct RasterPass<'graph, 'attachments> {
    graph: &'graph mut Graph,
    attachments: RasterAttachments<'attachments>,
    error: Option<GraphError>,
}

impl RasterPass<'_, '_> {
    /// Append an ordered batch of indexed triangle draws for one Raster Program.
    /// An empty batch still validates the Program's attachment contract.
    pub fn draw<I, D>(&mut self, program: &ManagedRasterProgram, draws: I) -> Result<(), GraphError>
    where
        I: IntoIterator<Item = D>,
        D: Borrow<RasterDraw>,
    {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        self.capture(program, draws).inspect_err(|error| {
            self.error = Some(error.clone());
        })
    }

    fn capture<I, D>(&mut self, program: &ManagedRasterProgram, draws: I) -> Result<(), GraphError>
    where
        I: IntoIterator<Item = D>,
        D: Borrow<RasterDraw>,
    {
        self.validate_attachments(program)?;
        let graph = &mut *self.graph;
        let first_draw = graph.draws.len();
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.graph.raster.capture_draws"),
                0,
            )
        });
        for draw in draws {
            let draw = draw.borrow();
            if graph.draws.len() == graph.capacity.draws {
                return Err(capacity_error(
                    GraphCapacityKind::Draws,
                    graph.capacity.draws,
                    graph.draws.len().checked_add(1),
                ));
            }
            if draw.parameters.words().len() * 4 != program.parameter_size() as usize {
                return Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "Raster inline constants do not match the Program ABI",
                ));
            }
            let bindings = graph.use_raster_bindings(program, draw.bindings)?;
            let (vertices, indices) = graph.raster_buffers(
                draw.vertices,
                draw.indices,
                draw.index_count,
                u64::from(program.vertex_stride()),
            )?;
            let vs = graph.resources[vertices].desc.units();
            let ix = u64::from(draw.index_count) * 4;
            let data = graph.data_mut();
            for (resource, purpose, size) in [(vertices, 0, vs), (indices, 1, ix)] {
                if data.raster_ranges[resource] == [0; 2] {
                    // A validated resource is registered once, so this list
                    // fits the same prepared resource capacity as the ranges.
                    data.raster_resources.push(resource);
                }
                let range = &mut data.raster_ranges[resource][purpose];
                *range = (*range).max(size);
            }
            data.draws.push(RasterDrawOperands {
                bindings,
                vertices,
                indices,
                index_count: draw.index_count,
                parameters: draw.parameters,
            });
        }
        let draws = first_draw..graph.draws.len();
        if !draws.is_empty() {
            // Each stored batch owns at least one draw, so the batch arena
            // reserved at the draw limit always has capacity here.
            graph.data_mut().raster_batches.push(RasterBatchOperands {
                program: *program,
                draws,
            });
        }
        Ok(())
    }

    fn validate_attachments(&self, program: &ManagedRasterProgram) -> Result<(), GraphError> {
        if program.requires_depth_load()
            && !self
                .attachments
                .depth
                .is_some_and(|depth| matches!(depth.load, LoadOp::Load))
        {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "Raster Program requires a depth attachment with Load",
            ));
        }
        if program.requires_depth() && self.attachments.depth.is_none() {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "Raster Program requires a depth attachment",
            ));
        }
        if self.attachments.colors.len() != program.color_outputs().len() {
            return Err(GraphError::new(
                GraphErrorKind::Unsupported,
                "Raster attachment count differs from the Program output contract",
            ));
        }
        for (attachment, format) in self.attachments.colors.iter().zip(program.color_outputs()) {
            let resource = self.graph.image_id(attachment.view.image)?;
            if self.graph.image_desc_at(resource)?.format != *format {
                return Err(self.graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "Raster attachment format differs from the Program output contract",
                    resource,
                ));
            }
        }
        Ok(())
    }
}

impl Graph {
    /// Build an ordered Raster scope using one set of attachments, load/store
    /// actions, and optional color resolves. The callback is consumed during
    /// declaration. Its draws, Programs and resources are owned by the Graph.
    /// If no draws were recorded, the scope becomes an attachment-only pass.
    pub fn raster_pass(
        &mut self,
        name: &str,
        attachments: RasterAttachments<'_>,
        record: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
    ) -> Result<PassHandle, GraphError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("renderer.graph.raster"), 0));
        self.build_pass(name, |graph| {
            let first_draw = graph.draws.len();
            let first_batch = graph.raster_batches.len();
            let first_access = graph.accesses.len();
            graph.data_mut().clear_raster_ranges();
            let mut raster = RasterPass {
                graph,
                attachments,
                error: None,
            };
            let result = record(&mut raster);
            if let Some(error) = raster.error {
                return Err(error);
            }
            result?;

            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!("renderer.graph.raster.accesses_and_attachments"),
                    0,
                )
            });
            let has_shader_accesses = graph.accesses.len() != first_access;
            graph.append_raster_buffer_accesses()?;
            let plan = graph.append_attachments(attachments)?;
            // Only shader-accessed images can overlap a color, depth or resolve
            // target. Vertex and index inputs are read-only buffers.
            let checked = if has_shader_accesses {
                first_access..graph.accesses.len()
            } else {
                plan.accesses.clone()
            };
            if let Some(left) = graph.conflicting_pass_access(checked.clone())? {
                let access = &graph.accesses[checked.start + left];
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "overlapping Raster shader and attachment footprints are unsupported",
                        access.resource,
                    )
                    .with_range(graph.access_range(access.resource, access.range.clone())));
            }
            let batches = first_batch..graph.raster_batches.len();
            let operation = if batches.is_empty() {
                Operation::AttachmentPass {
                    colors: plan.colors,
                    depth: plan.depth,
                }
            } else {
                Operation::Raster {
                    batches,
                    draws: first_draw..graph.draws.len(),
                    colors: plan.colors,
                    depth: plan.depth,
                }
            };
            Ok((operation, first_access..graph.accesses.len(), false))
        })
    }

    /// A single-program Raster scope. Indexed draw operands are captured in
    /// caller order from borrowed or owned inputs.
    pub fn raster<I, D>(
        &mut self,
        name: &str,
        program: &ManagedRasterProgram,
        draws: I,
        attachments: RasterAttachments<'_>,
    ) -> Result<PassHandle, GraphError>
    where
        I: IntoIterator<Item = D>,
        D: Borrow<RasterDraw>,
    {
        self.raster_pass(name, attachments, |raster| raster.draw(program, draws))
    }

    fn append_raster_buffer_accesses(&mut self) -> Result<(), GraphError> {
        // Preserve resource-order accesses without scanning unused resources.
        self.data_mut().raster_resources.sort_unstable();
        for index in 0..self.raster_resources.len() {
            let resource = self.raster_resources[index];
            let [vertices, indices] = self.raster_ranges[resource];
            if vertices != 0 {
                self.append_access(read(resource, 0..vertices, AccessPurpose::Vertex))?;
            }
            if indices != 0 {
                self.append_access(read(resource, 0..indices, AccessPurpose::Index))?;
            }
        }
        Ok(())
    }

    fn raster_buffers(
        &self,
        vertices: BufferHandle,
        indices: BufferHandle,
        index_count: u32,
        stride: u64,
    ) -> Result<(usize, usize), GraphError> {
        let vertices = self.buffer_id(vertices)?;
        let indices = self.buffer_id(indices)?;
        let vs = self.resources[vertices].desc.units();
        let ix = u64::from(index_count) * 4;
        if vs < stride
            || !vs.is_multiple_of(stride)
            || index_count == 0
            || !index_count.is_multiple_of(3)
            || ix > self.resources[indices].desc.units()
        {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "raster requires complete vertex strides and nonzero triangle u32 indices within the index buffer",
                if vs < stride || !vs.is_multiple_of(stride) { vertices } else { indices },
            ));
        }
        Ok((vertices, indices))
    }
}

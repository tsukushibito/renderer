//! Ordered graphics commands recorded into one attachment scope.

use super::builder::read;
use super::storage::capacity_error;
use super::{
    AccessPurpose, Graph, GraphCapacityKind, GraphError, GraphErrorKind, IndexedIndirectArgs,
    IndirectDrawOperands, LoadOp, Operation, PassHandle, RasterAttachments, RasterBatchOperands,
    RasterBindings, RasterConstants, RasterDraw, RasterDrawOperands,
};
use crate::program::{INDIRECT_COMMAND_BYTES, ManagedRasterProgram};
use std::{borrow::Borrow, ops::Range};

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

    /// Append indexed commands encoded by this Graph. All commands in the
    /// batch share one Program, geometry pair, binding set and constants.
    /// Any indices in the complete prefix covered by the template ranges
    /// must be initialized and valid for the vertex buffer.
    pub fn draw_indirect(
        &mut self,
        program: &ManagedRasterProgram,
        arguments: IndexedIndirectArgs,
        bindings: Option<RasterBindings>,
        parameters: RasterConstants,
    ) -> Result<(), GraphError> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        self.capture_indirect(program, arguments, bindings, parameters)
            .inspect_err(|error| {
                self.error = Some(error.clone());
            })
    }

    fn capture_indirect(
        &mut self,
        program: &ManagedRasterProgram,
        arguments: IndexedIndirectArgs,
        bindings: Option<RasterBindings>,
        parameters: RasterConstants,
    ) -> Result<(), GraphError> {
        self.validate_attachments(program)?;
        let graph = &mut *self.graph;
        let buffer = graph.buffer_id(arguments.buffer)?;
        let first_draw = graph.draws.len();
        // The templates validate the topology of each actual draw. The
        // representative draw covers their entire prefix for GPU preflight.
        graph.capture_raster_draw(
            program,
            &RasterDraw {
                vertices: arguments.vertices,
                indices: arguments.indices,
                first_index: 0,
                index_count: arguments.max_index_end,
                instance_count: 1,
                bindings,
                parameters,
            },
        )?;
        graph.append_access(read(
            buffer,
            0..u64::from(arguments.command_count) * INDIRECT_COMMAND_BYTES,
            AccessPurpose::Indirect,
        ))?;
        graph.data_mut().raster_batches.push(RasterBatchOperands {
            program: *program,
            draws: first_draw..first_draw + 1,
            indirect: Some(IndirectDrawOperands {
                buffer,
                command_count: arguments.command_count,
            }),
        });
        Ok(())
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
            if !draw.index_count.is_multiple_of(3) {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "Raster requires complete triangles",
                    graph.buffer_id(draw.indices)?,
                ));
            }
            graph.capture_raster_draw(program, draw)?;
        }
        let draws = first_draw..graph.draws.len();
        if !draws.is_empty() {
            // Each stored batch owns at least one draw, so the batch arena
            // reserved at the draw limit always has capacity here.
            graph.data_mut().raster_batches.push(RasterBatchOperands {
                program: *program,
                draws,
                indirect: None,
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
    fn capture_raster_draw(
        &mut self,
        program: &ManagedRasterProgram,
        draw: &RasterDraw,
    ) -> Result<(), GraphError> {
        if self.draws.len() == self.capacity.draws {
            return Err(capacity_error(
                GraphCapacityKind::Draws,
                self.capacity.draws,
                self.draws.len().checked_add(1),
            ));
        }
        if draw.parameters.words().len() * 4 != program.parameter_size() as usize {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "Raster inline constants do not match the Program ABI",
            ));
        }
        let bindings = self.use_raster_bindings(program, draw.bindings)?;
        let (vertices, indices, index_range) =
            self.raster_buffers(draw, u64::from(program.vertex_stride()))?;
        let vertex_bytes = self.resources[vertices].desc.units();
        let data = self.data_mut();
        data.track_raster_prefix(vertices, 0, vertex_bytes);
        if draw.first_index == 0 {
            data.track_raster_prefix(indices, 1, index_range.end);
        } else {
            // One scratch range per stored draw fits the reserved draw limit.
            data.raster_index_ranges.push((indices, index_range));
        }
        data.draws.push(RasterDrawOperands {
            bindings,
            vertices,
            indices,
            first_index: draw.first_index,
            index_count: draw.index_count,
            instance_count: draw.instance_count,
            parameters: draw.parameters,
        });
        Ok(())
    }

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
        if !self.raster_index_ranges.is_empty() {
            self.append_ranged_index_accesses()?;
        }
        Ok(())
    }

    fn append_ranged_index_accesses(&mut self) -> Result<(), GraphError> {
        self.data_mut()
            .raster_index_ranges
            .sort_unstable_by_key(|(resource, range)| (*resource, range.start));
        let mut pending = None::<(usize, Range<u64>)>;
        for index in 0..self.raster_index_ranges.len() {
            let (resource, mut range) = self.raster_index_ranges[index].clone();
            // The offset-zero fast path has already emitted this prefix.
            range.start = range.start.max(self.raster_ranges[resource][1]);
            if range.is_empty() {
                continue;
            }
            if let Some((previous_resource, previous)) = &mut pending
                && *previous_resource == resource
                && range.start <= previous.end
            {
                previous.end = previous.end.max(range.end);
                continue;
            }
            if let Some((previous_resource, previous)) = pending.replace((resource, range)) {
                self.append_access(read(previous_resource, previous, AccessPurpose::Index))?;
            }
        }
        if let Some((resource, range)) = pending {
            self.append_access(read(resource, range, AccessPurpose::Index))?;
        }
        Ok(())
    }

    fn raster_buffers(
        &self,
        draw: &RasterDraw,
        stride: u64,
    ) -> Result<(usize, usize, Range<u64>), GraphError> {
        let vertices = self.buffer_id(draw.vertices)?;
        let indices = self.buffer_id(draw.indices)?;
        let vs = self.resources[vertices].desc.units();
        if vs < stride || !vs.is_multiple_of(stride) {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "Raster requires complete vertex strides",
                vertices,
            ));
        }
        let start = u64::from(draw.first_index) * 4;
        let end = start + u64::from(draw.index_count) * 4;
        if draw.index_count == 0 || end > self.resources[indices].desc.units() {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "Raster requires a nonempty u32 index range within the index buffer",
                indices,
            ));
        }
        if draw.instance_count == 0 {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "Raster requires a nonzero instance count",
            ));
        }
        Ok((vertices, indices, start..end))
    }
}

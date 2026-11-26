//! Ordered graphics commands recorded into one attachment scope.

use super::builder::read;
use super::pass::IndexedDrawOperands;
use super::storage::capacity_error;
use super::{
    AccessPurpose, Binding, Graph, GraphCapacityKind, GraphError, GraphErrorKind,
    IndexedIndirectArgs, IndirectDrawOperands, LoadOp, Operation, PassHandle, RasterAttachments,
    RasterBatchOperands, RasterBindings, RasterConstants, RasterDraw, RasterDrawOperands,
    RasterRect, RasterRegions,
};
use crate::program::{INDIRECT_COMMAND_BYTES, ManagedRasterProgram};
use std::{borrow::Borrow, ops::Range};

mod generated;
use super::resource::BufferLookup;
pub(super) use generated::CaptureJobs;

/// A temporary recorder for ordered draw batches in one Raster attachment scope.
/// On error the enclosing pass is discarded, including any earlier batches.
pub struct RasterPass<'graph, 'attachments> {
    graph: &'graph mut Graph,
    attachments: RasterAttachments<'attachments>,
    regions: RasterRegions,
    error: Option<GraphError>,
}

impl RasterPass<'_, '_> {
    /// Map subsequent draws into this rectangle. The viewport must have positive
    /// width and height and fit the selected attachment mip. Depth remains 0..1.
    /// `None` restores the full attachment, the default on entry to every pass.
    /// Load, clear, store and resolve still apply to the complete attachment.
    pub fn set_viewport(&mut self, rect: Option<RasterRect>) -> Result<(), GraphError> {
        self.check_rect(rect, true)?;
        self.regions.viewport = rect;
        Ok(())
    }

    /// Clip subsequent draws to a rectangle inside the selected attachment mip.
    /// An empty rectangle clips every fragment. `None` restores the full
    /// attachment. Changing the scissor leaves the current viewport in effect.
    pub fn set_scissor(&mut self, rect: Option<RasterRect>) -> Result<(), GraphError> {
        self.check_rect(rect, false)?;
        self.regions.scissor = rect;
        Ok(())
    }

    fn check_rect(&mut self, rect: Option<RasterRect>, viewport: bool) -> Result<(), GraphError> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let Some(rect) = rect else { return Ok(()) };
        self.validate_rect(rect, viewport).inspect_err(|error| {
            self.error = Some(error.clone());
        })
    }

    fn validate_rect(&self, rect: RasterRect, viewport: bool) -> Result<(), GraphError> {
        let view = self
            .attachments
            .colors
            .first()
            .map(|a| a.view)
            .or_else(|| self.attachments.depth.map(|a| a.view))
            .ok_or_else(|| GraphError::invalid_input("Raster rectangle requires an attachment"))?;
        let extent = self
            .graph
            .image_desc(view.image())?
            .mip_extent(view.mip_level())
            .expect("immutable view has a valid mip extent");
        // Both native APIs use signed scissor coordinates and positive-height viewports.
        let fits = |offset: u32, size: u32, limit: u32| {
            offset
                .checked_add(size)
                .is_some_and(|end| end <= limit.min(i32::MAX as u32))
        };
        if viewport && (rect.width == 0 || rect.height == 0) {
            return Err(GraphError::invalid_input(
                "Raster viewport must have positive dimensions",
            ));
        }
        if !fits(rect.x, rect.width, extent[0]) || !fits(rect.y, rect.height, extent[1]) {
            return Err(GraphError::invalid_input(
                "Raster rectangle exceeds attachment or native coordinate bounds",
            ));
        }
        Ok(())
    }
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

    /// Append one generated fullscreen triangle. The Program supplies its
    /// position from vertex IDs 0 through 2 and declares no vertex stream.
    /// Graph owns the bindings and inline constants. The draw uses the same
    /// attachments and ordering as other draws recorded in this pass.
    pub fn draw_fullscreen(
        &mut self,
        program: &ManagedRasterProgram,
        bindings: &[Binding],
        parameters: RasterConstants,
    ) -> Result<(), GraphError> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        self.capture_fullscreen(program, bindings, parameters)
            .inspect_err(|error| {
                self.error = Some(error.clone());
            })
    }

    fn capture_fullscreen(
        &mut self,
        program: &ManagedRasterProgram,
        bindings: &[Binding],
        parameters: RasterConstants,
    ) -> Result<(), GraphError> {
        if !program.is_fullscreen() {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "fullscreen draw requires a generated-vertex Raster Program",
            ));
        }
        self.validate_attachments(program)?;
        let graph = &mut *self.graph;
        let first_draw = graph.data().draws.len();
        let constants_start = graph.data().raster_constant_words.len();
        let words = parameters.words();
        graph.check_raster_draw_storage(program, words)?;
        let bindings = graph.capture_fullscreen_bindings(program, bindings)?;
        let data = graph.data_mut();
        data.draws.push(RasterDrawOperands {
            bindings,
            indexed: None,
        });
        data.raster_constant_words.extend_from_slice(words);
        data.raster_batches.push(RasterBatchOperands {
            program: program.identity(),
            regions: self.regions,
            draws: first_draw..first_draw + 1,
            constants_start,
            constant_words_per_draw: words.len(),
            indirect: None,
        });
        Ok(())
    }

    fn capture_indirect(
        &mut self,
        program: &ManagedRasterProgram,
        arguments: IndexedIndirectArgs,
        bindings: Option<RasterBindings>,
        parameters: RasterConstants,
    ) -> Result<(), GraphError> {
        Self::require_indexed_program(program)?;
        self.validate_attachments(program)?;
        let graph = &mut *self.graph;
        let buffer = graph.buffer_id(arguments.buffer)?;
        let first_draw = graph.data().draws.len();
        let constants_start = graph.data().raster_constant_words.len();
        // The templates validate the topology of each actual draw. The
        // representative draw covers their entire prefix for GPU preflight.
        graph.capture_raster_draw(
            program,
            &RasterDraw {
                secondary_vertices: arguments.secondary_vertices,

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
            program: program.identity(),
            regions: self.regions,
            draws: first_draw..first_draw + 1,
            constants_start,
            constant_words_per_draw: program.parameter_size() as usize / size_of::<u32>(),
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
        let started = self
            .graph
            .data()
            .raster_metrics
            .as_ref()
            .map(|_| std::time::Instant::now());
        Self::require_indexed_program(program)?;
        self.validate_attachments(program)?;
        let graph = &mut *self.graph;
        let first_draw = graph.data().draws.len();
        let constants_start = graph.data().raster_constant_words.len();
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.graph.raster.capture_draws"),
                0,
            )
        });
        for draw in draws {
            let draw = draw.borrow();
            graph.buffer_lookup().check_triangles(draw)?;
            graph.capture_raster_draw(program, draw)?;
        }
        let draws = first_draw..graph.data().draws.len();
        if !draws.is_empty() {
            // Each stored batch owns at least one draw, so the batch arena
            // reserved at the draw limit always has capacity here.
            graph.data_mut().raster_batches.push(RasterBatchOperands {
                program: program.identity(),
                regions: self.regions,
                draws,
                constants_start,
                constant_words_per_draw: program.parameter_size() as usize / size_of::<u32>(),
                indirect: None,
            });
        }
        if let Some(started) = started {
            graph
                .data_mut()
                .raster_metrics
                .as_mut()
                .unwrap()
                .streaming_capture += started.elapsed();
        }
        Ok(())
    }

    fn require_indexed_program(program: &ManagedRasterProgram) -> Result<(), GraphError> {
        if program.is_fullscreen() {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "indexed draw requires a Raster Program with a vertex stream",
            ));
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
        let words = draw.parameters.words();
        self.check_raster_draw_storage(program, words)?;
        let indexed = self.buffer_lookup().indexed_draw(program, draw)?;
        let bindings = self.use_raster_bindings(program, draw.bindings)?;
        self.register_indexed_reads(indexed, indexed.byte_range());
        let data = self.data_mut();
        data.draws.push(RasterDrawOperands {
            bindings,
            indexed: Some(indexed),
        });
        if !words.is_empty() {
            data.raster_constant_words.extend_from_slice(words);
        }
        Ok(())
    }

    fn check_raster_draw_storage(
        &self,
        program: &ManagedRasterProgram,
        words: &[u32],
    ) -> Result<(), GraphError> {
        let storage = self.data();
        if storage.draws.len() == storage.capacity.draws {
            return Err(capacity_error(
                GraphCapacityKind::Draws,
                storage.capacity.draws,
                storage.draws.len().checked_add(1),
            ));
        }
        if size_of_val(words) != program.parameter_size() as usize {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "Raster inline constants do not match the Program ABI",
            ));
        }
        if !words.is_empty() {
            let required = storage.raster_constant_words.len().checked_add(words.len());
            if required.is_none_or(|required| required > storage.raster_constant_word_limit) {
                return Err(capacity_error(
                    GraphCapacityKind::RasterConstantWords,
                    storage.raster_constant_word_limit,
                    required,
                ));
            }
        }
        Ok(())
    }

    /// Build an ordered Raster scope using one set of attachments, load/store
    /// actions, and optional color resolves. The callback is consumed during
    /// declaration. Its draws, Programs and resources are owned by the Graph.
    /// If no draws were recorded, the scope becomes an attachment-only pass.
    pub fn raster(
        &mut self,
        name: &str,
        attachments: RasterAttachments<'_>,
        record: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
    ) -> Result<PassHandle, GraphError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("renderer.graph.raster"), 0));
        self.build_pass(name, |graph| {
            let first_draw = graph.data().draws.len();
            let first_batch = graph.data().raster_batches.len();
            let first_access = graph.data().accesses.len();
            graph.data_mut().clear_raster_ranges();
            let mut raster = RasterPass {
                graph,
                attachments,
                regions: RasterRegions::default(),
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
            let has_shader_accesses = graph.data().accesses.len() != first_access;
            let started = graph
                .data()
                .raster_metrics
                .as_ref()
                .map(|_| std::time::Instant::now());
            graph.append_raster_buffer_accesses()?;
            if let Some(started) = started {
                graph
                    .data_mut()
                    .raster_metrics
                    .as_mut()
                    .unwrap()
                    .finalize_reads += started.elapsed();
            }
            let plan = graph.append_attachments(attachments)?;
            // Only shader-accessed images can overlap a color, depth or resolve
            // target. Vertex and index inputs are read-only buffers.
            let checked = if has_shader_accesses {
                first_access..graph.data().accesses.len()
            } else {
                plan.accesses.clone()
            };
            if let Some(left) = graph.conflicting_pass_access(checked.clone())? {
                let access = &graph.data().accesses[checked.start + left];
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "overlapping Raster shader and attachment footprints are unsupported",
                        access.resource,
                    )
                    .with_range(graph.access_range(access.resource, access.range.clone())));
            }
            let batches = first_batch..graph.data().raster_batches.len();
            let operation = if batches.is_empty() {
                Operation::AttachmentPass {
                    colors: plan.colors,
                    depth: plan.depth,
                }
            } else {
                Operation::Raster {
                    batches,
                    draws: first_draw..graph.data().draws.len(),
                    colors: plan.colors,
                    depth: plan.depth,
                }
            };
            Ok((operation, first_access..graph.data().accesses.len(), false))
        })
    }

    fn append_raster_buffer_accesses(&mut self) -> Result<(), GraphError> {
        // Preserve resource-order accesses without scanning unused resources.
        self.data_mut().raster_resources.sort_unstable();
        for index in 0..self.data().raster_resources.len() {
            let resource = self.data().raster_resources[index];
            let [vertices, indices] = self.data().raster_ranges[resource];
            if vertices != 0 {
                self.append_access(read(resource, 0..vertices, AccessPurpose::Vertex))?;
            }
            if indices != 0 {
                self.append_access(read(resource, 0..indices, AccessPurpose::Index))?;
            }
        }
        if !self.data().raster_index_ranges.is_empty() {
            self.append_ranged_index_accesses()?;
        }
        Ok(())
    }

    fn append_ranged_index_accesses(&mut self) -> Result<(), GraphError> {
        self.data_mut()
            .raster_index_ranges
            .sort_unstable_by_key(|(resource, range)| (*resource, range.start));
        let mut pending = None::<(usize, Range<u64>)>;
        for index in 0..self.data().raster_index_ranges.len() {
            let (resource, mut range) = self.data().raster_index_ranges[index].clone();
            // The offset-zero fast path has already emitted this prefix.
            range.start = range.start.max(self.data().raster_ranges[resource][1]);
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

    fn register_indexed_reads(&mut self, draw: IndexedDrawOperands, range: Range<u64>) {
        let data = self.data_mut();
        if let Some(secondary) = draw.secondary_vertices {
            data.track_raster_prefix(secondary, 0, data.resources[secondary].desc.units());
        }
        data.track_raster_prefix(draw.vertices, 0, data.resources[draw.vertices].desc.units());
        if range.start == 0 {
            data.track_raster_prefix(draw.indices, 1, range.end);
        } else {
            data.raster_index_ranges.push((draw.indices, range));
        }
    }
}

impl IndexedDrawOperands {
    fn byte_range(self) -> Range<u64> {
        let start = u64::from(self.first_index) * 4;
        start..start + u64::from(self.index_count) * 4
    }
}

impl BufferLookup<'_> {
    fn check_triangles(&self, draw: &RasterDraw) -> Result<(), GraphError> {
        if !draw.index_count.is_multiple_of(3) {
            self.buffer_id(draw.indices)?;
            return Err(self.invalid(draw.indices, "Raster requires complete triangles"));
        }
        Ok(())
    }

    fn indexed_draw(
        &self,
        program: &ManagedRasterProgram,
        draw: &RasterDraw,
    ) -> Result<IndexedDrawOperands, GraphError> {
        let vertices = self.buffer_id(draw.vertices)?;
        let indices = self.buffer_id(draw.indices)?;
        let vertex_bytes = self.resources[vertices].desc.units();
        let stride = u64::from(program.vertex_stride());
        if vertex_bytes < stride || !vertex_bytes.is_multiple_of(stride) {
            return Err(self.invalid(draw.vertices, "Raster requires complete vertex strides"));
        }
        let indexed = IndexedDrawOperands {
            vertices,
            indices,
            secondary_vertices: draw
                .secondary_vertices
                .map(|v| self.buffer_id(v))
                .transpose()?,
            first_index: draw.first_index,
            index_count: draw.index_count,
            instance_count: draw.instance_count,
        };
        if indexed.index_count == 0
            || indexed.byte_range().end > self.resources[indices].desc.units()
        {
            return Err(self.invalid(
                draw.indices,
                "Raster requires a nonempty u32 index range within the index buffer",
            ));
        }
        if draw.instance_count == 0 {
            return Err(GraphError::invalid_input(
                "Raster requires a nonzero instance count",
            ));
        }
        let secondary_stride = u64::from(program.descriptor().secondary_vertex_stride);
        match indexed.secondary_vertices {
            Some(id)
                if secondary_stride != 0
                    && self.resources[id]
                        .desc
                        .units()
                        .is_multiple_of(secondary_stride)
                    && self.resources[id].desc.units() / secondary_stride
                        == vertex_bytes / stride => {}
            None if secondary_stride == 0 => {}
            _ => {
                return Err(GraphError::invalid_input(
                    "secondary vertex buffer does not match the program and primary vertex count",
                ));
            }
        }
        Ok(indexed)
    }
}

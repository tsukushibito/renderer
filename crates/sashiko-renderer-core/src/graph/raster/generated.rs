//! Parallel capture into disjoint portions of the final operand and constant arenas.
use super::*;
use crate::jobs::RendererJobs;
use std::{
    num::NonZeroUsize,
    sync::{Mutex, TryLockError},
};

pub(in crate::graph) struct CaptureJobs {
    team: RendererJobs,
    minimum: NonZeroUsize,
    chunks: Mutex<Vec<Result<Footprint, GraphError>>>,
}

impl CaptureJobs {
    pub(in crate::graph) fn new(team: RendererJobs, minimum: NonZeroUsize) -> Self {
        let chunks = (0..team.parallelism().get())
            .map(|_| Ok(Footprint::default()))
            .collect();
        Self {
            team,
            minimum,
            chunks: Mutex::new(chunks),
        }
    }
}

/// A common geometry/binding run needs at most a prefix and a contiguous
/// nonzero range. More varied input is merged from the final ordered operands.
#[derive(Default)]
struct Footprint {
    mixed: bool,
    prefix: u64,
    range: Option<Range<u64>>,
}

impl Footprint {
    fn add(
        &mut self,
        first: &RasterDrawOperands,
        value: &RasterDrawOperands,
        indexed: IndexedDrawOperands,
    ) {
        if self.mixed {
            return;
        }
        let before = first.indexed.expect("generated indexed run");
        if before.vertices != indexed.vertices
            || before.indices != indexed.indices
            || before.secondary_vertices != indexed.secondary_vertices
            || first.bindings != value.bindings
        {
            self.mixed = true;
            return;
        }
        let range = indexed.byte_range();
        if range.start == 0 {
            self.prefix = self.prefix.max(range.end);
        } else if let Some(previous) = &mut self.range {
            if previous.start <= range.end && range.start <= previous.end {
                previous.start = previous.start.min(range.start);
                previous.end = previous.end.max(range.end);
            } else {
                self.mixed = true;
            }
        } else {
            self.range = Some(range);
        }
    }
}

impl RasterPass<'_, '_> {
    /// Append an ordered indexed run of known length. A configured Graph pool
    /// may evaluate different ordinals simultaneously. The producer must map
    /// each ordinal to its draw independently of invocation order.
    pub fn draw_generated<D: Borrow<RasterDraw>>(
        &mut self,
        program: &ManagedRasterProgram,
        count: usize,
        draw_at: impl Fn(usize) -> D + Sync,
    ) -> Result<(), GraphError> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        // A medium run can still benefit from coalescing its resource reads
        // when dispatching workers would cost more than it saves.
        const MIN_INLINE_DRAWS: usize = 384;
        let config = self
            .graph
            .raster_jobs
            .as_ref()
            .filter(|config| count >= config.minimum.get().min(MIN_INLINE_DRAWS))
            .cloned();
        let result = if let Some(config) = &config
            && count >= config.minimum.get()
        {
            // Independent graph checkouts or nested producers can share a team.
            // The busy caller keeps its workspace; this checkout can progress inline.
            let chunks = match config.chunks.try_lock() {
                Ok(guard) => Some(guard),
                Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
                Err(TryLockError::WouldBlock) => None,
            };
            if let Some(mut chunks) = chunks {
                self.capture_generated(program, count, &draw_at, Some(&config.team), &mut chunks)
            } else {
                self.capture(program, (0..count).map(draw_at))
            }
        } else if config.is_some() {
            self.capture_generated(
                program,
                count,
                &draw_at,
                None,
                &mut [Ok(Footprint::default())],
            )
        } else {
            self.capture(program, (0..count).map(draw_at))
        };
        result.inspect_err(|e| self.error = Some(e.clone()))?;
        if let Some(maximum) = self.graph.generated_batch_size
            && count > maximum.get()
        {
            // Each batch owns at least one draw. The existing draw bound
            // guarantees capacity for every resulting batch.
            let batches = &mut self.graph.data_mut().raster_batches;
            let batch = batches.pop().expect("nonempty generated run");
            let groups = count.div_ceil(maximum.get());
            let width = count / groups;
            let mut consumed = 0;
            for group in 0..groups {
                let len = width + usize::from(group < count % groups);
                let start = batch.draws.start + consumed;
                batches.push(RasterBatchOperands {
                    draws: start..start + len,
                    constants_start: batch.constants_start
                        + consumed * batch.constant_words_per_draw,
                    ..batch
                });
                consumed += len;
            }
        }
        Ok(())
    }

    fn capture_generated<D: Borrow<RasterDraw>>(
        &mut self,
        program: &ManagedRasterProgram,
        count: usize,
        draw_at: &(impl Fn(usize) -> D + Sync),
        team: Option<&RendererJobs>,
        chunks: &mut [Result<Footprint, GraphError>],
    ) -> Result<(), GraphError> {
        let started = self
            .graph
            .raster_metrics
            .as_ref()
            .map(|_| std::time::Instant::now());
        Self::require_indexed_program(program)?;
        self.validate_attachments(program)?;
        let graph = &mut *self.graph;
        let data = graph.data_mut();
        let first = data.draws.len();
        let constants_start = data.raster_constant_words.len();
        let stride = program.parameter_size() as usize / size_of::<u32>();
        let end = first
            .checked_add(count)
            .filter(|n| *n <= data.capacity.draws)
            .ok_or_else(|| {
                capacity_error(
                    GraphCapacityKind::Draws,
                    data.capacity.draws,
                    first.checked_add(count),
                )
            })?;
        let required = count
            .checked_mul(stride)
            .and_then(|n| n.checked_add(constants_start));
        let word_end = required
            .filter(|n| *n <= data.raster_constant_word_limit)
            .ok_or_else(|| {
                capacity_error(
                    GraphCapacityKind::RasterConstantWords,
                    data.raster_constant_word_limit,
                    required,
                )
            })?;
        let chunk_size = count.div_ceil(chunks.len());
        let chunks = &mut chunks[..count.div_ceil(chunk_size)];
        let input = BufferLookup::new(data.id, &data.resources, &data.names);
        data.draws.resize(
            end,
            RasterDrawOperands {
                bindings: 0..0,
                indexed: None,
            },
        );
        data.raster_constant_words.resize(word_end, 0);
        let outputs = &mut data.draws[first..end];
        let words = &mut data.raster_constant_words[constants_start..word_end];

        #[cfg(feature = "tracy")]
        let capture_span = tracy_client::Client::running().map(|c| {
            c.span(
                tracy_client::span_location!("renderer.graph.raster.generated_operands"),
                0,
            )
        });
        let write_chunks = |jobs: &mut crate::jobs::JobSystem| {
            jobs.scope(|scope| {
                let mut words = words;
                for (group, (output, result)) in
                    outputs.chunks_mut(chunk_size).zip(&mut *chunks).enumerate()
                {
                    let (constants, tail) = words.split_at_mut(output.len() * stride);
                    words = tail;
                    let start = group * chunk_size;
                    let input = &input;
                    scope.spawn(move || {
                        *result = fill(input, program, draw_at, start, output, constants, stride);
                    });
                }
            })
        };
        if let Some(team) = team {
            team.with(write_chunks);
        } else {
            write_chunks(&mut crate::jobs::JobSystem::inline());
        }
        #[cfg(feature = "tracy")]
        drop(capture_span);
        #[cfg(feature = "tracy")]
        let _merge_span = tracy_client::Client::running().map(|c| {
            c.span(
                tracy_client::span_location!("renderer.graph.raster.generated_accesses"),
                0,
            )
        });

        let merging = started.map(|started| {
            let now = std::time::Instant::now();
            let metrics = graph.data_mut().raster_metrics.as_mut().unwrap();
            metrics.generated_capture += now - started;
            now
        });
        for (group, result) in chunks.iter_mut().enumerate() {
            let summary = std::mem::replace(result, Ok(Footprint::default()))?;
            let start = first + group * chunk_size;
            if summary.mixed {
                for n in start..start.saturating_add(chunk_size).min(end) {
                    let operand = &graph.draws[n];
                    let indexed = operand.indexed.expect("generated indexed run");
                    let bindings = operand.bindings.clone();
                    graph.register_raster_binding_reads(&bindings)?;
                    graph.register_indexed_reads(indexed, indexed.byte_range());
                }
            } else {
                let operand = &graph.draws[start];
                let indexed = operand.indexed.expect("generated indexed run");
                let bindings = operand.bindings.clone();
                graph.register_raster_binding_reads(&bindings)?;
                if summary.prefix > 0 {
                    graph.register_indexed_reads(indexed, 0..summary.prefix);
                }
                if let Some(range) = summary.range {
                    graph.register_indexed_reads(indexed, range);
                }
            }
        }
        graph.data_mut().raster_batches.push(RasterBatchOperands {
            program: program.identity(),
            regions: self.regions,
            draws: first..end,
            constants_start,
            constant_words_per_draw: stride,
            indirect: None,
        });
        if let Some(merging) = merging {
            let metrics = graph.data_mut().raster_metrics.as_mut().unwrap();
            metrics.generated_merge += merging.elapsed();
            metrics.generated_runs += 1;
            metrics.generated_draws += count;
        }
        Ok(())
    }
}

fn fill<D: Borrow<RasterDraw>>(
    lookup: &BufferLookup<'_>,
    program: &ManagedRasterProgram,
    draw_at: &(impl Fn(usize) -> D + Sync),
    start: usize,
    output: &mut [RasterDrawOperands],
    mut constants: &mut [u32],
    stride: usize,
) -> Result<Footprint, GraphError> {
    let mut summary = Footprint::default();
    let mut first = None;
    for (n, dst) in output.iter_mut().enumerate() {
        let source = draw_at(start + n);
        let draw = source.borrow();
        let words = draw.parameters.words();
        if words.len() != stride {
            return Err(GraphError::invalid_input(
                "Raster inline constants do not match the Program ABI",
            ));
        }
        lookup.check_triangles(draw)?;
        let indexed = lookup.indexed_draw(program, draw)?;
        *dst = RasterDrawOperands {
            indexed: Some(indexed),
            bindings: RasterBindings::resolve(draw.bindings, lookup.id, program)?,
        };
        summary.add(first.get_or_insert_with(|| dst.clone()), dst, indexed);
        let (owned, tail) = constants.split_at_mut(stride);
        owned.copy_from_slice(words);
        constants = tail;
    }
    Ok(summary)
}

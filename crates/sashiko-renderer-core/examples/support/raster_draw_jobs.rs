use std::{error::Error, num::NonZeroUsize, ops::Range};

use sashiko_jobs::JobSystem;
use sashiko_renderer_core::graph::{RasterConstants, RasterDraw};

pub(super) struct DrawPreparation {
    jobs: Option<JobSystem>,
    packets: Vec<RasterDraw>,
    count: usize,
}

impl DrawPreparation {
    pub(super) fn new(workers: usize, draws: usize) -> Result<Self, Box<dyn Error>> {
        let mut packets = Vec::new();
        let jobs = if let Some(participants) = NonZeroUsize::new(workers) {
            packets.try_reserve_exact(draws)?;
            Some(JobSystem::new(participants)?)
        } else {
            None
        };
        Ok(Self {
            jobs,
            packets,
            count: draws,
        })
    }

    /// Ranges partition all draw ordinals in pass and batch declaration order.
    pub(super) fn prepare(
        &mut self,
        template: RasterDraw,
        parameters: Option<&[RasterConstants]>,
        ranges: &[Range<usize>],
    ) {
        let Some(jobs) = &mut self.jobs else { return };
        // The first checkout supplies valid Graph-local handles to initialize the
        // reserved storage. Every checkout rewrites them before issuing any draw.
        self.packets.resize(self.count, template);
        let packets = self.packets.as_mut_slice();
        let template = &template;
        jobs.scope(|scope| {
            let mut remaining = packets;
            for range in ranges {
                let (batch, rest) = remaining.split_at_mut(range.len());
                remaining = rest;
                let first = range.start;
                let constants = parameters.map(|all| &all[range.clone()]);
                // Borrow the template so the capture fits in the scheduler's inline slot.
                scope.spawn(move || fill_batch(batch, template, first, constants));
            }
        });
    }

    pub(super) fn draws(&self, range: Range<usize>) -> &[RasterDraw] {
        &self.packets[range]
    }
}

fn fill_batch(
    batch: &mut [RasterDraw],
    template: &RasterDraw,
    first: usize,
    parameters: Option<&[RasterConstants]>,
) {
    if let Some(parameters) = parameters {
        for (packet, &parameters) in batch.iter_mut().zip(parameters) {
            *packet = RasterDraw {
                parameters,
                ..*template
            };
        }
    } else {
        for (i, packet) in batch.iter_mut().enumerate() {
            *packet = RasterDraw {
                first_index: ((first + i) * 3) as u32,
                ..*template
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sashiko_renderer_core::graph::{BufferDesc, GraphCapacity, GraphPool};

    #[test]
    fn uneven_batches_refresh_handles_and_keep_draw_order() {
        let ranges = [0..3, 3..7, 7..9];
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        for workers in [1, 2, 4] {
            let mut preparation = DrawPreparation::new(workers, 9).unwrap();
            for variant in 0..3 {
                let mut graph = pool.begin().unwrap();
                let buffer = BufferDesc { size: 128 };
                let template = RasterDraw {
                    vertices: graph.buffer("vertices", buffer).unwrap(),
                    indices: graph.buffer("indices", buffer).unwrap(),
                    first_index: 0,
                    index_count: 3,
                    instance_count: 1,
                    bindings: None,
                    secondary_vertices: None,
                    parameters: RasterConstants::default(),
                };
                let constants = std::array::from_fn::<_, 9, _>(|i| {
                    RasterConstants::new(&[variant, i as u32]).unwrap()
                });
                let parameters = (variant > 0).then_some(constants.as_slice());
                preparation.prepare(template, parameters, &ranges);
                for (i, packet) in preparation.draws(0..9).iter().enumerate() {
                    assert_eq!(
                        *packet,
                        RasterDraw {
                            first_index: if variant == 0 { 3 * i as u32 } else { 0 },
                            parameters: parameters
                                .map_or_else(RasterConstants::default, |all| all[i]),
                            ..template
                        }
                    );
                }
            }
        }
    }
}

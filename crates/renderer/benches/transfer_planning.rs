//! CPU-only comparison of copy hazards and initialized-range bookkeeping.
//!
//! Input generation and final planner destruction are outside the timed routine.
//! Allocation, overlap/initialization queries, insertion, and barrier decisions
//! are timed. Each iteration plans N gapped writes followed by N reads of them.
//! This does not create Vulkan objects, copy bytes, or time GPU execution.

use std::collections::HashMap;
use std::hint::black_box;
use std::ops::Range;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

// Compile the production algorithm without exposing renderer internals or
// enabling the native Vulkan backend just to benchmark CPU planning.
#[path = "../src/backends/vulkan/transfer/batch/hazards.rs"]
#[allow(
    dead_code,
    unused_imports,
    reason = "the benchmark includes private CPU code without its unit-test harness"
)]
mod hazards;
#[path = "../src/backends/vulkan/ranges.rs"]
#[allow(
    dead_code,
    unused_imports,
    reason = "the benchmark includes private CPU code without its unit-test harness"
)]
mod ranges;
use hazards::{BufferAccess, BufferHazards};
use ranges::ByteRanges;

const COUNTS: [usize; 4] = [8, 128, 1024, 8192];

#[derive(Clone, Copy)]
enum Order {
    Ascending,
    Descending,
    Shuffled,
}

impl Order {
    fn name(self) -> &'static str {
        match self {
            Self::Ascending => "ascending",
            Self::Descending => "descending",
            Self::Shuffled => "shuffled",
        }
    }
}

fn disjoint_writes(count: usize, order: Order) -> Vec<BufferAccess> {
    let mut offsets: Vec<_> = (0..count).collect();
    match order {
        Order::Ascending => {}
        Order::Descending => offsets.reverse(),
        Order::Shuffled => {
            // Fixed-seed Fisher-Yates permutation keeps the workload reproducible
            // without adding a random-number dependency to the benchmark.
            let mut state = 0x6a09_e667_f3bc_c909_u64;
            for index in (1..offsets.len()).rev() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                offsets.swap(index, (state % (index as u64 + 1)) as usize);
            }
        }
    }
    offsets
        .into_iter()
        .map(|index| {
            let offset = index as u64 * 16;
            BufferAccess {
                resource: 0,
                // Gaps prevent adjacent-range merging from hiding the scaling
                // problem; the algorithm must also handle nonadjacent ranges.
                range: offset..offset + 4,
                write: true,
            }
        })
        .collect()
}

/// The previous production algorithm, retained only as a measurement reference.
#[derive(Default)]
struct LinearReference(HashMap<usize, Vec<BufferAccess>>);

impl LinearReference {
    fn before(&mut self, accesses: &[BufferAccess]) -> bool {
        let needed = accesses.iter().any(|next| {
            self.0.get(&next.resource).is_some_and(|earlier| {
                earlier.iter().any(|prior| {
                    (prior.write || next.write)
                        && prior.range.start < next.range.end
                        && next.range.start < prior.range.end
                })
            })
        });
        if needed {
            self.0.clear();
        }
        for access in accesses {
            self.0
                .entry(access.resource)
                .or_default()
                .push(access.clone());
        }
        needed
    }
}

/// The previous initialized-range representation, paired with the old scanner.
#[derive(Default)]
struct LinearInitialized(Vec<Range<u64>>);

impl LinearInitialized {
    fn contains(&self, range: &Range<u64>) -> bool {
        range.is_empty()
            || self
                .0
                .iter()
                .any(|known| known.start <= range.start && known.end >= range.end)
    }

    fn insert(&mut self, mut range: Range<u64>) {
        if range.is_empty() {
            return;
        }
        let first = self.0.partition_point(|known| known.end < range.start);
        let mut after = first;
        while after < self.0.len() && self.0[after].start <= range.end {
            range.start = range.start.min(self.0[after].start);
            range.end = range.end.max(self.0[after].end);
            after += 1;
        }
        self.0.splice(first..after, [range]);
    }
}

#[derive(Default)]
struct LinearPlanner {
    hazards: LinearReference,
    initialized: LinearInitialized,
}

#[derive(Default)]
struct OrderedPlanner {
    hazards: BufferHazards,
    initialized: ByteRanges,
}

fn transfer_planning(c: &mut Criterion) {
    for order in [Order::Ascending, Order::Descending, Order::Shuffled] {
        let mut group = c.benchmark_group(format!("copy_range_planning/{}", order.name()));
        for count in COUNTS {
            let accesses = disjoint_writes(count, order);
            group.throughput(Throughput::Elements(count as u64 * 2));
            group.bench_with_input(
                BenchmarkId::new("linear_reference", count),
                &accesses,
                |b, accesses| {
                    b.iter_batched_ref(
                        LinearPlanner::default,
                        |planner| {
                            for access in black_box(accesses) {
                                black_box(planner.hazards.before(std::slice::from_ref(access)));
                                planner.initialized.insert(access.range.clone());
                            }
                            for access in black_box(accesses) {
                                black_box(planner.initialized.contains(&access.range));
                                let read = BufferAccess {
                                    write: false,
                                    ..access.clone()
                                };
                                black_box(planner.hazards.before(std::slice::from_ref(&read)));
                            }
                        },
                        // Each finished planner retains all ranges until the
                        // batch ends. Bound their accumulated memory footprint.
                        BatchSize::LargeInput,
                    );
                },
            );
            group.bench_with_input(
                BenchmarkId::new("ordered_ranges", count),
                &accesses,
                |b, accesses| {
                    b.iter_batched_ref(
                        OrderedPlanner::default,
                        |planner| {
                            for access in black_box(accesses) {
                                black_box(planner.hazards.before(std::slice::from_ref(access)));
                                planner.initialized.insert(access.range.clone());
                            }
                            for access in black_box(accesses) {
                                black_box(planner.initialized.contains(&access.range));
                                let read = BufferAccess {
                                    write: false,
                                    ..access.clone()
                                };
                                black_box(planner.hazards.before(std::slice::from_ref(&read)));
                            }
                        },
                        BatchSize::LargeInput,
                    );
                },
            );
        }
        group.finish();
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(30)
        .warm_up_time(Duration::from_millis(250))
        .measurement_time(Duration::from_secs(1));
    targets = transfer_planning
}
criterion_main!(benches);

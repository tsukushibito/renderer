//! Copy access tracking with ordered, disjoint unions of read and write ranges.
//!
//! This module has no Vulkan dependencies so its actual planning code can also
//! be exercised by the CPU-only benchmark without exposing backend APIs.

use std::collections::HashMap;
use std::ops::Range;

use super::ByteRanges;

#[derive(Clone)]
pub(super) struct BufferAccess {
    pub(super) resource: usize,
    pub(super) range: Range<u64>,
    pub(super) write: bool,
}

#[derive(Default)]
struct ResourceAccesses {
    reads: ByteRanges,
    writes: ByteRanges,
}

impl ResourceAccesses {
    fn conflicts(&self, access: &BufferAccess) -> bool {
        self.writes.overlaps(&access.range) || (access.write && self.reads.overlaps(&access.range))
    }

    fn insert(&mut self, access: &BufferAccess) {
        if access.write {
            self.writes.insert(access.range.clone());
        } else {
            self.reads.insert(access.range.clone());
        }
    }
}

/// Only copy hazards within this batch, not a second RenderGraph. Each resource
/// has separate unions for reads and writes, retaining the exact hazard set.
/// A global dependency retires all resources. Disjoint accesses cost O(log n)
/// each regardless of insertion order; coalescing removes each old range once.
#[derive(Default)]
pub(super) struct BufferHazards(HashMap<usize, ResourceAccesses>);

impl BufferHazards {
    pub(super) fn before(&mut self, accesses: &[BufferAccess]) -> bool {
        // Every range is nonempty and bounded by the copy builder. Check the
        // complete command against previous commands before inserting any of
        // its accesses, so one copy's source/destination do not self-conflict.
        let needed = accesses.iter().any(|access| {
            self.0
                .get(&access.resource)
                .is_some_and(|prior| prior.conflicts(access))
        });
        if needed {
            self.0.clear();
        }
        for access in accesses {
            self.0.entry(access.resource).or_default().insert(access);
        }
        needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(resource: usize, range: Range<u64>, write: bool) -> BufferAccess {
        BufferAccess {
            resource,
            range,
            write,
        }
    }

    #[test]
    fn reads_can_merge_without_introducing_false_hazards_across_gaps() {
        let mut hazards = BufferHazards::default();
        for range in [16..24, 0..8, 8..12, 20..32, 4..10] {
            assert!(!hazards.before(&[access(0, range, false)]));
        }
        assert!(!hazards.before(&[access(0, 12..16, true)]));
        assert!(hazards.before(&[access(0, 11..13, true)]));
        // A global barrier retires the earlier reads and writes together.
        assert!(!hazards.before(&[access(0, 16..32, true)]));
    }

    #[test]
    fn same_command_accesses_are_checked_before_any_are_inserted() {
        let mut hazards = BufferHazards::default();
        assert!(!hazards.before(&[access(0, 0..8, false), access(0, 16..24, true),]));
        assert!(!hazards.before(&[access(0, 8..16, true)]));
        assert!(hazards.before(&[access(0, 24..32, false), access(0, 0..8, true),]));
        assert!(!hazards.before(&[access(0, 16..24, false)]));
        assert!(hazards.before(&[access(0, 28..32, true)]));
    }

    #[test]
    fn disjoint_gapped_writes_remain_independent_in_every_order() {
        const COUNT: u64 = 4096;
        let ascending: Vec<_> = (0..COUNT).collect();
        let descending: Vec<_> = (0..COUNT).rev().collect();
        // Odd multiplication permutes a power-of-two-sized set, providing a
        // reproducible non-monotonic order without a random-number dependency.
        let shuffled: Vec<_> = (0..COUNT).map(|i| (i * 2053) % COUNT).collect();
        for order in [ascending, descending, shuffled] {
            let mut hazards = BufferHazards::default();
            for index in order {
                assert!(!hazards.before(&[access(0, index * 16..index * 16 + 4, true)]));
            }
            assert!(!hazards.before(&[access(0, 4..16, false)]));
            assert!(hazards.before(&[access(0, 3..5, false)]));
        }
    }

    #[test]
    fn ordered_hazard_decisions_match_direct_prior_access_comparison() {
        let mut indexed = BufferHazards::default();
        let mut prior: Vec<BufferAccess> = Vec::new();
        let mut state = 0x9723_8ad1_u64;
        for _ in 0..512 {
            // A bounded deterministic stream mixes adjacent, nested, gapped,
            // overlapping, and multi-resource accesses, including two accesses
            // to the same resource within one copy command.
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let start = (state >> 16) % 128;
                let end = start + 1 + ((state >> 32) % 32);
                access(((state >> 8) % 3) as usize, start..end, state & 4 != 0)
            };
            let command = [next(), next()];
            let expected = command.iter().any(|new| {
                prior.iter().any(|old| {
                    old.resource == new.resource
                        && (old.write || new.write)
                        && old.range.start < new.range.end
                        && new.range.start < old.range.end
                })
            });
            assert_eq!(indexed.before(&command), expected);
            if expected {
                prior.clear();
            }
            prior.extend(command);
        }
    }
}

//! Ordered unions of byte ranges shared by initialization and copy hazards.

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Included};
use std::ops::Range;

/// Nonempty, disjoint half-open intervals; adjacent intervals are merged.
/// Queries use one predecessor lookup. An insertion visits only intervals it
/// merges, giving O(N log N) total insertion work over N input ranges.
#[derive(Clone, Default)]
pub(super) struct ByteRanges(BTreeMap<u64, u64>);

impl ByteRanges {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    pub(super) fn contains(&self, range: &Range<u64>) -> bool {
        range.is_empty()
            || self
                .0
                .range(..=range.start)
                .next_back()
                .is_some_and(|(_, end)| *end >= range.end)
    }

    pub(super) fn overlaps(&self, range: &Range<u64>) -> bool {
        !range.is_empty()
            && self
                .0
                .range(..range.end)
                .next_back()
                .is_some_and(|(_, end)| *end > range.start)
    }

    pub(super) fn insert(&mut self, mut range: Range<u64>) {
        if range.is_empty() {
            return;
        }
        if let Some((&start, &end)) = self.0.range(..=range.start).next_back()
            && end >= range.start
        {
            if end >= range.end {
                return;
            }
            // Keep the predecessor's entry so extending a single interval does
            // not repeatedly allocate and free its BTreeMap node.
            range.start = start;
        }
        while let Some((&start, &end)) = self
            .0
            .range((Excluded(range.start), Included(range.end)))
            .next()
        {
            range.end = range.end.max(end);
            self.0.remove(&start);
        }
        self.0.insert(range.start, range.end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unions_preserve_holes_and_half_open_boundaries() {
        let mut ranges = ByteRanges::default();
        assert!(ranges.is_empty());
        assert!(ranges.contains(&(5..5)));
        assert!(!ranges.overlaps(&(5..5)));
        ranges.insert(5..5);
        assert!(ranges.is_empty());

        ranges.insert(8..12);
        ranges.insert(0..4);
        assert!(ranges.contains(&(1..3)));
        assert!(!ranges.contains(&(3..9)));
        assert!(ranges.overlaps(&(3..9)));
        assert!(!ranges.overlaps(&(4..8)));
        assert!(!ranges.overlaps(&(12..16)));

        ranges.insert(4..8);
        assert!(ranges.contains(&(0..12)));
        ranges.insert(2..3);
        assert!(!ranges.contains(&(0..13)));
        assert!(!ranges.overlaps(&(12..13)));

        let cloned = ranges.clone();
        ranges.clear();
        assert!(ranges.is_empty());
        assert!(cloned.contains(&(0..12)));
    }

    #[test]
    fn shuffled_disjoint_ranges_and_bridges_keep_exact_coverage() {
        let mut ranges = ByteRanges::default();
        // Multiplication permutes these indices, inserting into both sides of
        // existing gaps rather than exercising only ascending append behavior.
        for index in 0..257 {
            let start = ((index * 97) % 257) * 8;
            ranges.insert(start..start + 4);
        }
        for index in 0..257 {
            let start = index * 8;
            assert!(ranges.contains(&(start..start + 4)));
            assert!(ranges.overlaps(&(start + 3..start + 5)));
            assert!(!ranges.contains(&(start..start + 5)));
            assert!(!ranges.overlaps(&(start + 4..start + 8)));
        }
        ranges.insert(2..2054);
        assert!(ranges.contains(&(0..2054)));
        assert!(!ranges.overlaps(&(2054..2055)));

        ranges.insert(u64::MAX - 4..u64::MAX);
        assert!(ranges.contains(&(u64::MAX - 3..u64::MAX)));
        assert!(!ranges.overlaps(&(u64::MAX..u64::MAX)));
    }
}

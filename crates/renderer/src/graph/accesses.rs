//! Pass-local overlap detection, independent of the access representation.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    ops::Range,
};

/// Bounded exact-read index. Slots point at the compacted pass accesses; only
/// touched buckets are reset, independent of the prepared upper bound.
pub(super) struct ReadAccessSet {
    slots: Vec<usize>,
    touched: Vec<usize>,
    first: Vec<FirstRead>,
    resources: Vec<usize>,
}

#[derive(Clone, Copy)]
struct FirstRead {
    index: usize,
    hashed: bool,
}
impl FirstRead {
    const EMPTY: Self = Self {
        index: usize::MAX,
        hashed: false,
    };
}

impl ReadAccessSet {
    pub fn new(capacity: usize, resources: usize) -> Result<Self, super::GraphCapacityKind> {
        use super::GraphCapacityKind;
        let count = capacity
            .checked_mul(2)
            .and_then(|n| n.checked_add(1))
            .ok_or(GraphCapacityKind::Accesses)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(count)
            .map_err(|_| GraphCapacityKind::Accesses)?;
        slots.resize(count, usize::MAX);
        let mut touched = Vec::new();
        touched
            .try_reserve_exact(capacity)
            .map_err(|_| GraphCapacityKind::Accesses)?;
        let mut first = Vec::new();
        first
            .try_reserve_exact(resources)
            .map_err(|_| GraphCapacityKind::Resources)?;
        first.resize(resources, FirstRead::EMPTY);
        let mut used_resources = Vec::new();
        used_resources
            .try_reserve_exact(resources)
            .map_err(|_| GraphCapacityKind::Resources)?;
        Ok(Self {
            slots,
            touched,
            first,
            resources: used_resources,
        })
    }

    pub fn clear(&mut self) {
        for bucket in self.touched.drain(..) {
            self.slots[bucket] = usize::MAX;
        }
        for resource in self.resources.drain(..) {
            self.first[resource] = FirstRead::EMPTY;
        }
    }

    pub fn insert(
        &mut self,
        access: &super::ResourceAccess,
        destination: usize,
        accesses: &[super::ResourceAccess],
    ) -> bool {
        // Most resources have one exact read per pass. Retain the first record
        // directly and compare its repeats without hashing. Distinct ranges or
        // purposes/stages promote that resource into the general hash index;
        // no per-resource linear chain can become quadratic for many ranges.
        let first = self.first[access.resource];
        if first.index == usize::MAX {
            self.first[access.resource].index = destination;
            self.resources.push(access.resource);
            return true;
        }
        if accesses[first.index] == *access {
            return false;
        }
        if !first.hashed {
            self.insert_hashed(&accesses[first.index], first.index, accesses);
            self.first[access.resource].hashed = true;
        }
        self.insert_hashed(access, destination, accesses)
    }

    fn insert_hashed(
        &mut self,
        access: &super::ResourceAccess,
        destination: usize,
        accesses: &[super::ResourceAccess],
    ) -> bool {
        let mut hash = DefaultHasher::new();
        access.resource.hash(&mut hash);
        access.range.hash(&mut hash);
        access.read.hash(&mut hash);
        access.write.hash(&mut hash);
        access.initializes.hash(&mut hash);
        access.discard_after.hash(&mut hash);
        (access.purpose as u8).hash(&mut hash);
        for stage in [
            crate::program::ShaderStages::VERTEX,
            crate::program::ShaderStages::FRAGMENT,
            crate::program::ShaderStages::COMPUTE,
        ] {
            access.stages.contains(stage).hash(&mut hash);
        }
        let mut bucket = hash.finish() as usize % self.slots.len();
        loop {
            let previous = self.slots[bucket];
            if previous == usize::MAX {
                // At most capacity.accesses input records exist. The table is
                // larger than twice that bound and touched has that capacity.
                self.slots[bucket] = destination;
                self.touched.push(bucket);
                return true;
            }
            if accesses[previous] == *access {
                return false;
            }
            bucket = (bucket + 1) % self.slots.len();
        }
    }
}

#[derive(Clone, Copy)]
struct End {
    value: u64,
    index: usize,
}

#[derive(Clone, Copy, Default)]
struct Prefix {
    longest: Option<End>,
    second: Option<End>,
    writer: Option<End>,
}

impl Prefix {
    fn insert(&mut self, end: End, write: bool) {
        if self.longest.is_none_or(|longest| end.value > longest.value) {
            self.second = self.longest;
            self.longest = Some(end);
        } else if self.second.is_none_or(|second| end.value > second.value) {
            self.second = Some(end);
        }
        if write && self.writer.is_none_or(|writer| end.value > writer.value) {
            self.writer = Some(end);
        }
    }

    fn overlaps(self, index: usize, start: u64, write: bool) -> bool {
        let candidate = if write {
            self.longest
                .filter(|end| end.index != index)
                .or(self.second)
        } else {
            self.writer
        };
        candidate.is_some_and(|end| end.value > start)
    }
}

pub(super) struct IndexedAccess {
    resource: usize,
    range: Range<u64>,
    write: bool,
    index: usize,
    prefix: Prefix,
}

/// Return the first conflicting pair in original declaration order.
///
/// The caller establishes valid, nonempty ranges and access modes before this
/// check. `view` must describe the same immutable access on every call. This
/// helper establishes pass-local disjointness, not initialization or ordering
/// between passes; those guarantees remain the compiler's responsibility.
///
/// The sorted prefixes answer whether an interval overlaps any writer, or any
/// other access for a writer. Two longest ends exclude the queried writer itself.
/// Up to four writers use direct comparisons against the original declarations,
/// selecting the same lexicographically first pair without sorting the readers.
/// Otherwise original-order prefix queries and one final suffix scan preserve
/// the same pair and error attribution as a pairwise scan. A accesses use at
/// most O(A log A) time and O(A) prepared storage. All-reader passes need O(A)
/// time and no scratch storage.
#[cfg(test)]
pub(super) fn conflicting_accesses<T>(
    accesses: &[T],
    view: impl Fn(&T) -> (usize, Range<u64>, bool),
) -> Option<(usize, usize)> {
    let mut scratch = Vec::with_capacity(accesses.len());
    conflicting_accesses_in(accesses, view, &mut scratch).expect("prepared test scratch")
}

pub(super) fn conflicting_accesses_in<T>(
    accesses: &[T],
    view: impl Fn(&T) -> (usize, Range<u64>, bool),
    sorted: &mut Vec<IndexedAccess>,
) -> Result<Option<(usize, usize)>, ()> {
    conflicting_ranges_in(|| accesses.iter().map(&view), sorted)
}

/// The factory must replay the same immutable declarations in the same order.
/// Sharing the normalized iterator keeps builder and development diagnostics on
/// one overlap algorithm without projecting their inputs into another array.
pub(super) fn conflicting_ranges_in<I>(
    accesses: impl Fn() -> I,
    sorted: &mut Vec<IndexedAccess>,
) -> Result<Option<(usize, usize)>, ()>
where
    I: Iterator<Item = (usize, Range<u64>, bool)>,
{
    #[cfg(feature = "tracy")]
    let _span = tracy_client::Client::running().map(|client| {
        client.span(
            tracy_client::span_location!("renderer.graph.pass_overlap"),
            0,
        )
    });
    sorted.clear();
    // Raster commonly reads thousands of mesh/binding ranges and writes only
    // its attachments. Indexing/sorting every reader costs more than comparing
    // it with this small writer set. Retain original indices for error order.
    let mut count = 0;
    for (index, (resource, range, write)) in accesses().enumerate() {
        count = index + 1;
        if write {
            if sorted.len() == sorted.capacity() {
                return Err(());
            }
            sorted.push(IndexedAccess {
                resource,
                range,
                write,
                index,
                prefix: Prefix::default(),
            });
            if sorted.len() > 4 {
                break;
            }
        }
    }
    if sorted.len() <= 4 {
        if sorted.is_empty() {
            return Ok(None);
        }
        // Preserve the capacity contract for all declarations, not just writers.
        if count > sorted.capacity() {
            return Err(());
        }
        let mut conflict = None;
        for (index, (resource, range, _)) in accesses().enumerate() {
            for writer in sorted.iter() {
                if index != writer.index
                    && resource == writer.resource
                    && range.start < writer.range.end
                    && writer.range.start < range.end
                {
                    let pair = (index.min(writer.index), index.max(writer.index));
                    conflict = Some(conflict.map_or(pair, |previous| pair.min(previous)));
                }
            }
        }
        return Ok(conflict);
    }
    // Many writers retain the general O(A log A) interval index. Re-enumerate
    // from the start so indices and first-conflict attribution remain identical.
    sorted.clear();
    for (index, (resource, range, write)) in accesses().enumerate() {
        if sorted.len() == sorted.capacity() {
            return Err(());
        }
        sorted.push(IndexedAccess {
            resource,
            range,
            write,
            index,
            prefix: Prefix::default(),
        });
    }
    sorted.sort_unstable_by_key(|access| {
        (
            access.resource,
            access.range.start,
            access.range.end,
            access.index,
        )
    });
    let mut resource = None;
    let mut prefix = Prefix::default();
    for access in sorted.iter_mut() {
        if resource != Some(access.resource) {
            resource = Some(access.resource);
            prefix = Prefix::default();
        }
        prefix.insert(
            End {
                value: access.range.end,
                index: access.index,
            },
            access.write,
        );
        access.prefix = prefix;
    }
    for (left, (resource, range, write)) in accesses().enumerate() {
        let end = sorted.partition_point(|other| {
            other.resource < resource
                || (other.resource == resource && other.range.start < range.end)
        });
        let candidate = sorted[..end]
            .last()
            .filter(|other| other.resource == resource);
        if candidate.is_some_and(|other| other.prefix.overlaps(left, range.start, write)) {
            // The first original access with a conflict has no conflicting
            // predecessor: that predecessor would already have found this one.
            // Scan only its suffix to select the earliest second declaration.
            return Ok(accesses().enumerate().skip(left + 1).find_map(
                |(right, (other_resource, other_range, other_write))| {
                    (resource == other_resource
                        && range.start < other_range.end
                        && other_range.start < range.end
                        && (write || other_write))
                        .then_some((left, right))
                },
            ));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_read_index_preserves_stage_purpose_and_range_and_reuses_its_storage() {
        use crate::{
            graph::{AccessPurpose, ResourceAccess},
            program::ShaderStages,
        };
        let original = ResourceAccess {
            resource: 0,
            range: 0..16,
            read: true,
            write: false,
            initializes: false,
            discard_after: false,
            purpose: AccessPurpose::Uniform,
            stages: ShaderStages::VERTEX,
        };
        let accesses = [
            original.clone(),
            ResourceAccess {
                purpose: AccessPurpose::StorageRead,
                ..original.clone()
            },
            ResourceAccess {
                stages: ShaderStages::FRAGMENT,
                ..original.clone()
            },
            ResourceAccess {
                range: 4..16,
                ..original.clone()
            },
        ];
        let mut index = ReadAccessSet::new(accesses.len(), 1).unwrap();
        let slots = index.slots.as_ptr();
        let touched = index.touched.as_ptr();
        for _ in 0..3 {
            index.clear();
            for (at, access) in accesses.iter().enumerate() {
                assert!(index.insert(access, at, &accesses));
                assert!(
                    !index.insert(access, at, &accesses),
                    "only identical read declarations share a record"
                );
            }
            assert_eq!(index.touched.len(), accesses.len());
        }
        assert_eq!(index.slots.as_ptr(), slots);
        assert_eq!(index.touched.as_ptr(), touched);
    }

    #[test]
    fn read_compaction_promotes_variants_and_resets_between_passes() {
        use crate::{graph::ResourceAccess, program::ShaderStages};
        let first = ResourceAccess {
            resource: 63,
            range: 0..16,
            read: true,
            write: false,
            initializes: false,
            discard_after: false,
            purpose: super::super::AccessPurpose::Uniform,
            stages: ShaderStages::VERTEX,
        };
        let variants = [
            first.clone(),
            ResourceAccess {
                range: 4..16,
                ..first.clone()
            },
            ResourceAccess {
                stages: ShaderStages::FRAGMENT,
                ..first.clone()
            },
            ResourceAccess {
                resource: 0,
                ..first
            },
        ];
        let sequence = [0, 0, 1, 0, 1, 2, 3, 3, 2, 1];
        let mut index = ReadAccessSet::new(sequence.len(), 64).unwrap();
        let mut compacted = Vec::with_capacity(sequence.len());
        // A different first variant on reuse must not observe any prior pass's
        // compacted indices or hash entries. Resource count also exceeds A.
        for reverse in [false, true, false] {
            index.clear();
            compacted.clear();
            let mut expected = Vec::new();
            for step in 0..sequence.len() {
                let step = if reverse {
                    sequence.len() - 1 - step
                } else {
                    step
                };
                let access = &variants[sequence[step]];
                let unique = !expected.contains(access);
                assert_eq!(index.insert(access, compacted.len(), &compacted), unique);
                if unique {
                    expected.push(access.clone());
                    compacted.push(access.clone());
                }
            }
            assert_eq!(compacted, expected);
        }
    }

    #[derive(Clone)]
    struct Access {
        resource: usize,
        range: Range<u64>,
        write: bool,
    }

    fn access(resource: usize, range: Range<u64>, write: bool) -> Access {
        Access {
            resource,
            range,
            write,
        }
    }

    fn view(access: &Access) -> (usize, Range<u64>, bool) {
        (access.resource, access.range.clone(), access.write)
    }

    #[test]
    fn replayed_owned_ranges_preserve_conflicts_and_refuse_scratch_growth() {
        let declarations = || {
            (0..3).map(|index| {
                let range = if index == 2 {
                    2..6
                } else {
                    index * 4..index * 4 + 4
                };
                (0, range, index == 2)
            })
        };
        let mut short = Vec::with_capacity(2);
        let pointer = short.as_ptr();
        assert_eq!(conflicting_ranges_in(declarations, &mut short), Err(()));
        assert_eq!(short.as_ptr(), pointer);
        assert_eq!(short.capacity(), 2);

        let mut prepared = Vec::with_capacity(3);
        assert_eq!(
            conflicting_ranges_in(declarations, &mut prepared),
            Ok(Some((0, 2))),
        );
        let mut empty = Vec::new();
        assert_eq!(
            conflicting_ranges_in(|| (0..100).map(|_| (0, 0..8, false)), &mut empty),
            Ok(None),
        );
        assert_eq!(empty.capacity(), 0);
    }

    #[test]
    fn adjacent_ranges_and_overlapping_readers_are_independent() {
        assert_eq!(
            conflicting_accesses(
                &[
                    access(0, 0..4, true),
                    access(0, 4..8, true),
                    access(0, 8..16, false),
                    access(0, 8..12, false),
                    access(1, 0..16, true),
                ],
                view,
            ),
            None,
        );
    }

    #[test]
    fn sparse_and_dense_writers_keep_exact_pair_order_and_capacity_contract() {
        for writers in [1, 3, 4, 5] {
            let mut accesses = (0..96)
                .map(|index| access(0, index * 4..index * 4 + 4, false))
                .collect::<Vec<_>>();
            for index in 0..writers {
                accesses.push(access(1, index * 4..index * 4 + 4, true));
            }
            let mut scratch = Vec::with_capacity(accesses.len());
            let pointer = scratch.as_ptr();
            assert_eq!(
                conflicting_accesses_in(&accesses, view, &mut scratch),
                Ok(None),
            );
            // The first writer precedes its conflicting reader. A writer-only
            // existence query cannot itself choose this original-order pair.
            accesses[0] = access(1, 0..4, true);
            accesses[1] = access(1, 2..4, false);
            assert_eq!(
                conflicting_accesses_in(&accesses, view, &mut scratch),
                Ok(Some((0, 1))),
            );
            assert_eq!(scratch.as_ptr(), pointer);

            let mut short = Vec::with_capacity(accesses.len() - 1);
            assert_eq!(
                conflicting_accesses_in(&accesses, view, &mut short),
                Err(()),
                "the capacity bound includes readers, writers={writers}",
            );
        }
    }

    #[test]
    fn first_pair_precedes_resource_sort_and_write_write_priority() {
        let accesses = [
            access(5, 0..4, false),
            access(3, 0..4, true),
            access(5, 0..4, true),
            access(3, 0..4, true),
        ];
        assert_eq!(conflicting_accesses(&accesses, view), Some((0, 2)));
    }

    #[test]
    fn first_second_declaration_precedes_largest_overlapping_range() {
        let accesses = [
            access(0, 5..6, false),
            access(0, 5..6, true),
            access(0, 0..100, true),
        ];
        assert_eq!(conflicting_accesses(&accesses, view), Some((0, 1)));
    }

    #[test]
    fn nested_writes_find_the_earliest_declaration_without_self_conflicts() {
        let accesses = [
            access(0, 3..4, true),
            access(0, 200..204, true),
            access(0, 0..100, true),
            access(0, 1..2, true),
        ];
        assert_eq!(conflicting_accesses(&accesses, view), Some((0, 2)));
        assert_eq!(conflicting_accesses(&accesses[..1], view), None);
    }

    #[test]
    fn many_readers_require_one_projection_each_and_keep_conflict_attribution() {
        let mut accesses = vec![access(0, 0..64, false); 8_192];
        let projections = std::cell::Cell::new(0);
        assert_eq!(
            conflicting_accesses(&accesses, |access| {
                projections.set(projections.get() + 1);
                view(access)
            }),
            None,
        );
        assert_eq!(projections.get(), accesses.len());
        accesses.push(access(0, 32..64, true));
        assert_eq!(conflicting_accesses(&accesses, view), Some((0, 8_192)));
    }

    #[test]
    fn generated_inputs_preserve_the_pairwise_declaration_contract() {
        for seed in 0..4_096u64 {
            let mut state = seed;
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                state >> 32
            };
            let count = next() as usize % 33;
            let accesses = (0..count)
                .map(|_| {
                    let resource = next() as usize % 4;
                    let start = next() % 16 * 4;
                    let end = start + (1 + next() % ((64 - start) / 4)) * 4;
                    access(resource, start..end, next().is_multiple_of(2))
                })
                .collect::<Vec<_>>();
            let expected = accesses.iter().enumerate().find_map(|(left, access)| {
                accesses[left + 1..]
                    .iter()
                    .position(|other| {
                        access.resource == other.resource
                            && access.range.start < other.range.end
                            && other.range.start < access.range.end
                            && (access.write || other.write)
                    })
                    .map(|offset| (left, left + 1 + offset))
            });
            assert_eq!(
                conflicting_accesses(&accesses, view),
                expected,
                "seed={seed}",
            );
        }
    }
}

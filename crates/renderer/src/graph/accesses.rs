//! Pass-local overlap detection, independent of the access representation.

use std::ops::Range;

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
/// Original-order queries and one final suffix scan preserve the same pair and
/// error attribution as a pairwise scan. A accesses use O(A log A) time and O(A)
/// temporary storage. All-reader passes need O(A) time and no allocation.
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
    sorted.clear();
    if !accesses().any(|(_, _, write)| write) {
        return Ok(None);
    }
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

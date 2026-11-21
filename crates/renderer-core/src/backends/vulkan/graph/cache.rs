//! Event-maintained candidates for physical resource reuse. Pending and registered
//! entries are absent; acquisition never walks the rest of the registry.

#[cfg(test)]
use std::collections::BTreeSet;
use std::collections::TryReserveError;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

/// At most one pending notification per registry entry, even if callers hold
/// or drop many cloned leases without performing another acquisition.
pub(super) struct RecycleQueue(Mutex<RecycleState>);

struct RecycleState {
    queued: Vec<bool>,
    indices: Vec<usize>,
}

impl RecycleQueue {
    pub(super) fn new(capacity: usize) -> Result<Self, TryReserveError> {
        let mut queued = Vec::new();
        queued.try_reserve_exact(capacity)?;
        queued.resize(capacity, false);
        let mut indices = Vec::new();
        indices.try_reserve_exact(capacity)?;
        Ok(Self(Mutex::new(RecycleState { queued, indices })))
    }

    fn released(&self, index: usize) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if !state.queued[index] {
            state.queued[index] = true;
            // One queued entry per registry index fits the prepared capacity.
            state.indices.push(index);
        }
    }

    pub(super) fn grow(&self, capacity: usize) -> Result<(), TryReserveError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if capacity <= state.queued.len() {
            return Ok(());
        }
        let additional = capacity.saturating_sub(state.indices.len());
        state.indices.try_reserve_exact(additional)?;
        let additional = capacity.saturating_sub(state.queued.len());
        state.queued.try_reserve_exact(additional)?;
        state.queued.resize(capacity, false);
        Ok(())
    }

    pub(super) fn drain_into(&self, output: &mut Vec<usize>) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        assert!(
            output.capacity() >= state.queued.len(),
            "prepared recycle scratch capacity"
        );
        output.clear();
        let RecycleState { queued, indices } = &mut *state;
        // Visit only notifications, including with a large registered prefix.
        // Drain retains the prepared backing and duplicate releases use no slot.
        for index in indices.drain(..) {
            queued[index] = false;
            output.push(index);
        }
    }

    #[cfg(test)]
    pub(super) fn take(&self) -> BTreeSet<usize> {
        let capacity = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .queued
            .len();
        let mut result = Vec::with_capacity(capacity);
        self.drain_into(&mut result);
        result.into_iter().collect()
    }
}

/// Registry ownership is an ordinary Arc. Every escaped reference uses this
/// lease so its final release can make the entry available without a rescan.
pub(super) struct ResourceLease<T> {
    value: Option<Arc<T>>,
    index: usize,
    recycle: Arc<RecycleQueue>,
}

impl<T> ResourceLease<T> {
    pub(super) fn new(value: Arc<T>, index: usize, recycle: Arc<RecycleQueue>) -> Self {
        Self {
            value: Some(value),
            index,
            recycle,
        }
    }
}

impl<T> Clone for ResourceLease<T> {
    fn clone(&self) -> Self {
        Self::new(
            Arc::clone(self.value.as_ref().unwrap()),
            self.index,
            Arc::clone(&self.recycle),
        )
    }
}

impl<T> Deref for ResourceLease<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value.as_deref().expect("live resource lease")
    }
}

impl<T> Drop for ResourceLease<T> {
    fn drop(&mut self) {
        // Publish only after releasing the Arc. A concurrent collector must not
        // consume the final notification while the reference is still alive.
        let weak = Arc::downgrade(self.value.as_ref().unwrap());
        drop(self.value.take());
        // Only the registry may remain. If another lease is created or remains,
        // its eventual release will check again. The last concurrent release
        // observes its own preceding decrement before inspecting this count.
        if weak.strong_count() == 1 {
            self.recycle.released(self.index);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CacheTarget {
    Reuse(usize),
    Create,
    Replace(usize),
}

#[derive(Clone, Copy)]
struct CandidateClass<K> {
    descriptor: K,
    count: usize,
    // Lower bound for the next matching index. Returning a lower index moves
    // this cursor back; taking candidates only advances it.
    next: usize,
}

pub(super) struct CacheIndex<K> {
    records: Vec<Option<(K, u64)>>,
    // A bit per available registry index; removal never shifts other entries.
    available: Vec<u64>,
    // Descriptor classes persist while empty, so ordinary release/reacquire
    // cycles only update scalars. Publishing a previously unindexed descriptor
    // may move this small table; it never grows beyond prepared capacity.
    classes: Vec<CandidateClass<K>>,
    last_class: usize,
    count: usize,
}

impl<K: Copy + Ord> CacheIndex<K> {
    pub(super) fn new(capacity: usize) -> Result<Self, TryReserveError> {
        let mut records = Vec::new();
        records.try_reserve_exact(capacity)?;
        records.resize(capacity, None);
        let mut available = Vec::new();
        available.try_reserve_exact(capacity.div_ceil(64))?;
        available.resize(capacity.div_ceil(64), 0);
        let mut classes = Vec::new();
        classes.try_reserve_exact(capacity)?;
        Ok(Self {
            records,
            available,
            classes,
            last_class: usize::MAX,
            count: 0,
        })
    }

    /// Keep tentative selections private to an explicit preparation attempt.
    pub(super) fn for_preparation(&self) -> Result<Self, TryReserveError> {
        self.with_capacity(self.records.len())
    }

    pub(super) fn with_capacity(&self, capacity: usize) -> Result<Self, TryReserveError> {
        let mut scratch = Self::new(capacity.max(self.records.len()))?;
        scratch.records[..self.records.len()].copy_from_slice(&self.records);
        scratch.available[..self.available.len()].copy_from_slice(&self.available);
        scratch.classes.extend_from_slice(&self.classes);
        scratch.last_class = self.last_class;
        scratch.count = self.count;
        Ok(scratch)
    }

    fn class(&mut self, descriptor: K) -> Result<usize, usize> {
        if self
            .classes
            .get(self.last_class)
            .is_some_and(|class| class.descriptor == descriptor)
        {
            return Ok(self.last_class);
        }
        let result = self
            .classes
            .binary_search_by_key(&descriptor, |class| class.descriptor);
        if let Ok(class) = result {
            self.last_class = class;
        }
        result
    }

    pub(super) fn insert(&mut self, index: usize, descriptor: K, serial: u64) {
        self.remove(index);
        let class = match self.class(descriptor) {
            Ok(class) => class,
            Err(_) => {
                if self.classes.len() == self.records.len() {
                    // At least one class is empty: each nonempty class owns a
                    // different candidate, and this index is not inserted yet.
                    let empty = self
                        .classes
                        .iter()
                        .position(|class| class.count == 0)
                        .expect("prepared descriptor class capacity");
                    self.classes.remove(empty);
                }
                let position = self
                    .classes
                    .binary_search_by_key(&descriptor, |class| class.descriptor)
                    .unwrap_err();
                self.classes.insert(
                    position,
                    CandidateClass {
                        descriptor,
                        count: 0,
                        next: self.records.len(),
                    },
                );
                self.last_class = position;
                position
            }
        };
        let class = &mut self.classes[class];
        class.next = class.next.min(index);
        class.count += 1;
        self.count += 1;
        self.records[index] = Some((descriptor, serial));
        self.available[index / 64] |= 1 << (index % 64);
    }

    pub(super) fn remove(&mut self, index: usize) {
        if let Some((descriptor, _)) = self.records[index].take() {
            self.available[index / 64] &= !(1 << (index % 64));
            let class = self.class(descriptor).expect("indexed descriptor class");
            let class = &mut self.classes[class];
            class.count -= 1;
            self.count -= 1;
            if class.count == 0 {
                class.next = self.records.len();
            } else if class.next == index {
                class.next = index + 1;
            }
        }
    }

    fn matching(&mut self, descriptor: K) -> Option<usize> {
        let class = self.class(descriptor).ok()?;
        if self.classes[class].count == 0 {
            return None;
        }
        let start = self.classes[class].next;
        for word_index in start / 64..self.available.len() {
            let mut word = self.available[word_index];
            if word_index == start / 64 {
                word &= u64::MAX << (start % 64);
            }
            while word != 0 {
                let index = word_index * 64 + word.trailing_zeros() as usize;
                word &= word - 1;
                if self.records[index].unwrap().0 == descriptor {
                    self.classes[class].next = index;
                    return Some(index);
                }
            }
        }
        unreachable!("nonempty descriptor class has an available candidate")
    }

    fn oldest(&self) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        self.available
            .iter()
            .copied()
            .enumerate()
            .flat_map(|(word_index, mut word)| {
                std::iter::from_fn(move || {
                    if word == 0 {
                        return None;
                    }
                    let index = word_index * 64 + word.trailing_zeros() as usize;
                    word &= word - 1;
                    Some(index)
                })
            })
            .min_by_key(|&index| (self.records[index].unwrap().1, index))
    }

    /// Inspect only indexed candidates. Invalidated candidates are removed once;
    /// the owner must publish a new availability event before retrying them.
    /// Selection does not consume a valid victim, so failed native creation can
    /// leave the original allocation and its cache order untouched.
    pub(super) fn select(
        &mut self,
        descriptor: K,
        entry_count: usize,
        capacity: usize,
        vacant: Option<usize>,
        mut eligible: impl FnMut(usize) -> bool,
    ) -> Result<CacheTarget, ()> {
        while let Some(index) = self.matching(descriptor) {
            if eligible(index) {
                return Ok(CacheTarget::Reuse(index));
            }
            self.remove(index);
        }
        // A trimmed slot has no allocation to evict. Prefer it to growing the
        // registry or replacing an allocation of another descriptor.
        if let Some(index) = vacant {
            return Ok(CacheTarget::Replace(index));
        }
        if entry_count < capacity {
            return Ok(CacheTarget::Create);
        }
        // Return an eviction candidate only after matching reuse and available
        // registry capacity have been exhausted. The owner still decides
        // whether creation/replacement is allowed at this boundary.
        while let Some(index) = self.oldest() {
            if eligible(index) {
                return Ok(CacheTarget::Replace(index));
            }
            self.remove(index);
        }
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_events_follow_arc_release_and_repeat_after_the_last_clone() {
        let value = Arc::new(17);
        let recycle = Arc::new(RecycleQueue::new(16).unwrap());
        let first = ResourceLease::new(Arc::clone(&value), 3, Arc::clone(&recycle));
        let last = first.clone();
        drop(first);
        assert!(recycle.take().is_empty());
        assert_eq!(Arc::strong_count(&value), 2);
        drop(last);
        assert_eq!(recycle.take(), BTreeSet::from([3]));
        assert_eq!(Arc::strong_count(&value), 1);
        for _ in 0..1_024 {
            drop(ResourceLease::new(
                Arc::clone(&value),
                3,
                Arc::clone(&recycle),
            ));
        }
        assert_eq!(recycle.take(), BTreeSet::from([3]));
    }

    #[test]
    fn concurrent_last_lease_release_always_publishes_availability() {
        let value = Arc::new(17);
        let recycle = Arc::new(RecycleQueue::new(16).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let leases = (0..4)
            .map(|_| ResourceLease::new(Arc::clone(&value), 9, Arc::clone(&recycle)))
            .collect::<Vec<_>>();
        std::thread::scope(|scope| {
            for lease in leases {
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    drop(lease);
                });
            }
        });
        assert_eq!(Arc::strong_count(&value), 1);
        assert_eq!(recycle.take(), BTreeSet::from([9]));
    }

    #[test]
    fn candidates_preserve_descriptor_priority_growth_and_eviction_order() {
        let mut index = CacheIndex::new(32).unwrap();
        index.insert(4, 'a', 3);
        index.insert(2, 'a', 8);
        index.insert(1, 'b', 3);
        assert_eq!(
            index.select('a', 5, 6, None, |_| true),
            Ok(CacheTarget::Reuse(2))
        );
        assert_eq!(
            index.select('c', 5, 6, None, |_| true),
            Ok(CacheTarget::Create)
        );
        assert_eq!(
            index.select('c', 6, 6, None, |_| true),
            Ok(CacheTarget::Replace(1))
        );
        // Selecting does not evict: a failed native replacement can retry.
        assert_eq!(
            index.select('c', 6, 6, None, |_| true),
            Ok(CacheTarget::Replace(1))
        );
        index.remove(1);
        assert_eq!(
            index.select('c', 6, 6, None, |_| true),
            Ok(CacheTarget::Replace(4))
        );
        index.insert(4, 'a', 10);
        assert_eq!(
            index.select('c', 6, 6, None, |_| true),
            Ok(CacheTarget::Replace(2))
        );
    }

    #[test]
    fn vacant_slots_follow_matching_reuse_but_precede_growth_and_eviction() {
        let mut index = CacheIndex::new(32).unwrap();
        index.insert(2, 'a', 7);
        index.insert(1, 'b', 3);
        assert_eq!(
            index.select('a', 4, 8, Some(0), |_| true),
            Ok(CacheTarget::Reuse(2))
        );
        assert_eq!(
            index.select('c', 4, 8, Some(0), |_| true),
            Ok(CacheTarget::Replace(0)),
            "fill a trimmed slot before growing the registry"
        );
        assert_eq!(
            index.select('c', 4, 4, Some(0), |_| true),
            Ok(CacheTarget::Replace(0)),
            "fill a trimmed slot before evicting another descriptor"
        );
        // Candidate selection is transactional. A failed native creation has
        // consumed neither the supplied vacancy nor the indexed idle resources.
        assert_eq!(
            index.select('c', 4, 4, None, |_| true),
            Ok(CacheTarget::Replace(1))
        );
    }

    #[test]
    fn availability_events_match_a_registry_reference_across_state_changes() {
        #[derive(Clone, Copy)]
        struct Entry {
            descriptor: u8,
            serial: u64,
            available: bool,
        }
        let mut entries: Vec<Entry> = Vec::new();
        let mut index = CacheIndex::new(32).unwrap();
        let mut random = 7u64;
        for serial in 0..4_096 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let descriptor = (random >> 32) as u8 % 7;
            if !entries.is_empty() {
                let changed = (random as usize) % entries.len();
                let entry = &mut entries[changed];
                entry.available = !entry.available;
                if entry.available {
                    index.insert(changed, entry.descriptor, entry.serial);
                } else {
                    index.remove(changed);
                }
            }
            let expected = entries
                .iter()
                .position(|entry| entry.available && entry.descriptor == descriptor)
                .map(CacheTarget::Reuse)
                .or_else(|| (entries.len() < 32).then_some(CacheTarget::Create))
                .or_else(|| {
                    entries
                        .iter()
                        .enumerate()
                        .filter(|(_, entry)| entry.available)
                        .min_by_key(|&(position, entry)| (entry.serial, position))
                        .map(|(position, _)| CacheTarget::Replace(position))
                });
            let actual = index.select(descriptor, entries.len(), 32, None, |n| {
                entries[n].available
            });
            assert_eq!(actual.ok(), expected);
            if let Ok(target) = actual {
                let position = match target {
                    CacheTarget::Reuse(position) | CacheTarget::Replace(position) => {
                        index.remove(position);
                        position
                    }
                    CacheTarget::Create => {
                        entries.push(Entry {
                            descriptor,
                            serial,
                            available: false,
                        });
                        entries.len() - 1
                    }
                };
                entries[position] = Entry {
                    descriptor,
                    serial,
                    available: false,
                };
            }
        }
    }

    #[test]
    fn large_registered_prefix_and_many_leased_targets_do_not_get_rechecked() {
        let mut index = CacheIndex::new(17_408).unwrap();
        let registered = 16_384;
        let targets = 1_024;
        for position in registered..registered + targets {
            index.insert(position, 7, 1);
        }
        let mut checks = 0;
        for position in registered..registered + targets {
            let selected =
                index.select(7, registered + targets, registered + targets, None, |_| {
                    checks += 1;
                    true
                });
            assert_eq!(selected, Ok(CacheTarget::Reuse(position)));
            index.remove(position);
        }
        assert_eq!(checks, targets);
        // Every selected resource is now leased. Repeated capacity requests do
        // not revisit them; a future release event makes them candidates again.
        for _ in 0..targets {
            assert_eq!(
                index.select(7, registered + targets, registered + targets, None, |_| {
                    checks += 1;
                    false
                }),
                Err(())
            );
        }
        assert_eq!(checks, targets);
        index.insert(registered, 7, 2);
        assert_eq!(
            index.select(7, registered + targets, registered + targets, None, |_| {
                true
            }),
            Ok(CacheTarget::Reuse(registered))
        );
    }

    #[test]
    fn ineligible_candidates_are_checked_once_until_an_event_republishes_them() {
        let mut index = CacheIndex::new(32).unwrap();
        index.insert(0, 1, 0);
        index.insert(1, 1, 1);
        let mut checks = 0;
        for _ in 0..10 {
            assert_eq!(
                index.select(1, 2, 2, None, |_| {
                    checks += 1;
                    false
                }),
                Err(())
            );
        }
        assert_eq!(checks, 2);
        index.insert(1, 1, 1);
        assert_eq!(
            index.select(1, 2, 2, None, |_| true),
            Ok(CacheTarget::Reuse(1))
        );
    }

    #[test]
    fn returned_lower_indices_reset_cursors_across_bitmap_words() {
        let mut index = CacheIndex::new(193).unwrap();
        for position in [192, 130, 64, 63, 0] {
            index.insert(position, 7, 2);
        }
        for position in [0, 63, 64] {
            assert_eq!(
                index.select(7, 193, 193, None, |_| true),
                Ok(CacheTarget::Reuse(position))
            );
            index.remove(position);
        }
        // Returning a lower index must invalidate the advanced cursor, without
        // making any of the other still-leased entries available again.
        index.insert(63, 7, 3);
        assert_eq!(
            index.select(7, 193, 193, None, |_| true),
            Ok(CacheTarget::Reuse(63))
        );
        index.remove(63);
        assert_eq!(
            index.select(7, 193, 193, None, |position| position != 130),
            Ok(CacheTarget::Reuse(192))
        );
        index.remove(192);
        assert_eq!(index.select(7, 193, 193, None, |_| true), Err(()));
    }

    #[test]
    fn descriptor_and_serial_replacement_reuse_bounded_class_metadata() {
        let mut index = CacheIndex::new(4).unwrap();
        for position in 0..4 {
            index.insert(position, position, 1);
        }
        // Updating an existing available entry removes its previous class and
        // eviction metadata before publishing the replacement.
        index.insert(0, 3, 9);
        assert_eq!(
            index.select(3, 4, 4, None, |_| true),
            Ok(CacheTarget::Reuse(0))
        );
        assert_eq!(
            index.select(100, 4, 4, None, |_| true),
            Ok(CacheTarget::Replace(1))
        );
        for round in 0..100 {
            for position in 0..4 {
                index.remove(position);
            }
            for position in (0..4).rev() {
                index.insert(position, 10 + round * 4 + position, round as u64);
            }
            for position in 0..4 {
                assert_eq!(
                    index.select(10 + round * 4 + position, 4, 4, None, |_| true),
                    Ok(CacheTarget::Reuse(position))
                );
            }
        }
        assert_eq!(index.classes.len(), 4);
    }
}

//! Event-maintained candidates for physical resource reuse. Pending and registered
//! entries are absent; acquisition never walks the rest of the registry.

#[cfg(test)]
use std::collections::BTreeSet;
use std::collections::TryReserveError;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

/// At most one pending notification per registry entry, even if callers hold
/// or drop many cloned leases without performing another acquisition.
pub(super) struct RecycleQueue(Mutex<Vec<bool>>);

impl RecycleQueue {
    pub(super) fn new(capacity: usize) -> Result<Self, TryReserveError> {
        let mut queued = Vec::new();
        queued.try_reserve_exact(capacity)?;
        queued.resize(capacity, false);
        Ok(Self(Mutex::new(queued)))
    }

    fn released(&self, index: usize) {
        self.0.lock().unwrap_or_else(|error| error.into_inner())[index] = true;
    }

    pub(super) fn drain_into(&self, output: &mut Vec<usize>) {
        let mut queued = self.0.lock().unwrap_or_else(|error| error.into_inner());
        assert!(
            output.capacity() >= queued.len(),
            "prepared recycle scratch capacity"
        );
        output.clear();
        for (index, queued) in queued.iter_mut().enumerate() {
            if std::mem::take(queued) {
                output.push(index);
            }
        }
    }

    #[cfg(test)]
    pub(super) fn take(&self) -> BTreeSet<usize> {
        let capacity = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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

pub(super) struct CacheIndex<K> {
    records: Vec<Option<(K, u64)>>,
    // Stable registry indices in ascending order. Only indexed candidates are
    // visited; large registered/leased prefixes are absent from this list.
    candidates: Vec<usize>,
}

impl<K: Copy + Ord> CacheIndex<K> {
    pub(super) fn new(capacity: usize) -> Result<Self, TryReserveError> {
        let mut records = Vec::new();
        records.try_reserve_exact(capacity)?;
        records.resize(capacity, None);
        let mut candidates = Vec::new();
        candidates.try_reserve_exact(capacity)?;
        Ok(Self {
            records,
            candidates,
        })
    }

    pub(super) fn insert(&mut self, index: usize, descriptor: K, serial: u64) {
        self.records[index] = Some((descriptor, serial));
        if let Err(position) = self.candidates.binary_search(&index) {
            // At most one candidate per fixed registry index. Preparation
            // reserved every possible entry, so insert only moves elements.
            self.candidates.insert(position, index);
        }
    }

    pub(super) fn remove(&mut self, index: usize) {
        if self.records[index].take().is_some() {
            let position = self
                .candidates
                .binary_search(&index)
                .expect("indexed candidate");
            self.candidates.remove(position);
        }
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
        while let Some(index) = self
            .candidates
            .iter()
            .copied()
            .find(|&index| self.records[index].unwrap().0 == descriptor)
        {
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
        while let Some(index) = self
            .candidates
            .iter()
            .copied()
            .min_by_key(|&index| (self.records[index].unwrap().1, index))
        {
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
}

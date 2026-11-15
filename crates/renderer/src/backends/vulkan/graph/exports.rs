//! Prepared frame-export metadata, retained by cheap submission clones.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use super::{Export, RuntimeError};

struct Slot {
    entries: Mutex<Entries>,
    max_exports: usize,
}

struct Entries {
    values: Vec<(usize, Export)>,
    pool_alive: bool,
}

impl Slot {
    fn entries(&self) -> MutexGuard<'_, Entries> {
        // No application code runs under this lock. The private vector remains
        // memory-safe if an interrupted owner poisoned the bookkeeping lock.
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

pub(super) struct ExportPool {
    slots: Vec<Arc<Slot>>,
}

impl ExportPool {
    pub(super) fn new(max_exports: usize, slots: usize) -> Result<Self, RuntimeError> {
        if slots == 0 {
            return Err(RuntimeError::invalid("export pool needs at least one slot"));
        }
        let mut prepared = Vec::new();
        prepared
            .try_reserve_exact(slots)
            .map_err(|_| RuntimeError::capacity("cannot reserve export slots"))?;
        for _ in 0..slots {
            let mut entries = Vec::new();
            entries
                .try_reserve_exact(max_exports)
                .map_err(|_| RuntimeError::capacity("cannot reserve frame export metadata"))?;
            prepared.push(Arc::new(Slot {
                entries: Mutex::new(Entries {
                    values: entries,
                    pool_alive: true,
                }),
                max_exports,
            }));
        }
        Ok(Self { slots: prepared })
    }

    pub(super) fn acquire(&mut self) -> Result<ExportLease, RuntimeError> {
        for slot in &mut self.slots {
            if let Some(storage) = Arc::get_mut(slot) {
                storage
                    .entries
                    .get_mut()
                    .unwrap_or_else(|error| error.into_inner())
                    .values
                    .clear();
                return Ok(ExportLease {
                    slot: Some(Arc::clone(slot)),
                });
            }
        }
        Err(RuntimeError::capacity(
            "all prepared export slots are retained",
        ))
    }

    pub(super) fn is_idle(&mut self) -> bool {
        // Weak probes held by a returning lease must also finish before a new
        // checkout or pool replacement can claim exclusive backing ownership.
        self.slots
            .iter_mut()
            .all(|slot| Arc::get_mut(slot).is_some())
    }
}

impl Drop for ExportPool {
    fn drop(&mut self) {
        // A retained submission may outlive the pool. Publish detachment before
        // releasing any base Arc, under the same lock used by returning leases.
        // Otherwise strong_count==1 could mean one surviving external clone.
        for slot in &self.slots {
            slot.entries().pool_alive = false;
        }
    }
}

/// Submission cloning retains only the prepared slot Arc. All stored import
/// snapshots are released as soon as the final external lease returns.
#[derive(Clone)]
pub(super) struct ExportLease {
    slot: Option<Arc<Slot>>,
}

impl ExportLease {
    fn slot(&self) -> &Arc<Slot> {
        self.slot.as_ref().expect("live export lease")
    }

    pub(super) fn insert(&mut self, index: usize, export: Export) -> Result<(), RuntimeError> {
        let slot = self.slot();
        let mut entries = slot.entries();
        match entries
            .values
            .binary_search_by_key(&index, |(index, _)| *index)
        {
            Ok(position) => entries.values[position].1 = export,
            Err(position) => {
                if entries.values.len() == slot.max_exports {
                    return Err(RuntimeError::capacity(
                        "frame exports exceed prepared capacity",
                    ));
                }
                entries.values.insert(position, (index, export));
            }
        }
        Ok(())
    }

    pub(super) fn get(&self, index: usize) -> Option<Export> {
        let entries = self.slot().entries();
        entries
            .values
            .binary_search_by_key(&index, |(index, _)| *index)
            .ok()
            .map(|position| entries.values[position].1.clone())
    }

    pub(super) fn len(&self) -> usize {
        self.slot().entries().values.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for ExportLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExportLease")
            .field("len", &self.len())
            .finish()
    }
}

impl Drop for ExportLease {
    fn drop(&mut self) {
        let returning = Arc::downgrade(self.slot());
        drop(self.slot.take());
        if returning.strong_count() == 1 {
            // While the pool is alive, its base Arc is the sole remaining owner.
            // This Weak makes Arc::get_mut reject acquisition while we clear
            // retained ImportRanges tokens, including concurrent last returns.
            if let Some(slot) = returning.upgrade() {
                let mut entries = slot.entries();
                if entries.pool_alive {
                    entries.values.clear();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::RuntimeErrorKind;
    use super::*;
    use crate::graph::{
        BufferDesc, ImageDesc, ImportBuffer, ImportImage, ImportRanges, ImportRangesPool,
        ResourceKey,
    };

    fn image(revision: u64) -> Export {
        Export::Image(ImportImage {
            key: ResourceKey {
                owner: 1,
                index: 1,
                generation: 1,
            },
            revision,
            desc: ImageDesc::rgba8(1, 1),
            initialized: true,
        })
    }

    fn buffer(initialized: ImportRanges) -> Export {
        Export::Buffer(ImportBuffer {
            key: ResourceKey {
                owner: 1,
                index: 2,
                generation: 1,
            },
            revision: 1,
            desc: BufferDesc { size: 16 },
            initialized,
        })
    }

    #[test]
    fn bounded_slots_keep_clones_and_replace_existing_indices_without_growth() {
        assert!(ExportPool::new(0, 0).is_err());
        let mut empty = ExportPool::new(0, 1).unwrap();
        let mut lease = empty.acquire().unwrap();
        assert!(lease.is_empty());
        assert!(lease.insert(0, image(1)).is_err());
        drop(lease);
        assert!(empty.is_idle());

        let mut pool = ExportPool::new(2, 2).unwrap();
        let mut first = pool.acquire().unwrap();
        let second = pool.acquire().unwrap();
        first.insert(7, image(1)).unwrap();
        first.insert(2, image(2)).unwrap();
        first.insert(2, image(3)).unwrap();
        assert_eq!(first.len(), 2);
        assert!(matches!(first.get(2), Some(Export::Image(value)) if value.revision == 3));
        assert!(first.get(4).is_none());
        assert_eq!(
            first.insert(3, image(4)).unwrap_err().kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert!(matches!(first.get(7), Some(Export::Image(value)) if value.revision == 1));
        assert_eq!(
            pool.acquire().unwrap_err().kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        let retained = first.clone();
        drop((first, second));
        assert!(!pool.is_idle());
        let reclaimed = pool.acquire().unwrap();
        assert!(reclaimed.is_empty());
        assert!(matches!(retained.get(2), Some(Export::Image(value)) if value.revision == 3));
        drop((retained, reclaimed));
        assert!(pool.is_idle());
    }

    #[test]
    fn last_return_releases_import_tokens_before_another_export_capture() {
        let mut ranges = ImportRangesPool::new(1, 1).unwrap();
        let mut pool = ExportPool::new(1, 1).unwrap();
        let mut lease = pool.acquire().unwrap();
        lease
            .insert(
                2,
                buffer(ranges.capture(std::slice::from_ref(&(0..16))).unwrap()),
            )
            .unwrap();
        let retained = lease.clone();
        drop(lease);
        assert!(!ranges.is_idle());
        let snapshot = retained.get(2).unwrap();
        drop(retained);
        assert!(pool.is_idle());
        assert!(
            !ranges.is_idle(),
            "external import snapshots still retain their contents"
        );
        drop(snapshot);
        assert!(
            ranges.is_idle(),
            "idle export metadata must not pin old range tokens"
        );
        let mut lease = pool.acquire().unwrap();
        lease
            .insert(
                2,
                buffer(ranges.capture(std::slice::from_ref(&(4..8))).unwrap()),
            )
            .unwrap();
        drop(lease);
        assert!(
            ranges.is_idle(),
            "last return clears metadata without requiring another acquisition"
        );
        assert!(ranges.capture(std::slice::from_ref(&(8..12))).is_ok());
    }

    #[test]
    fn concurrent_last_returns_clear_tokens_before_unique_reacquisition() {
        use std::sync::Barrier;
        let mut ranges = ImportRangesPool::new(1, 1).unwrap();
        let mut pool = ExportPool::new(1, 1).unwrap();
        for _ in 0..64 {
            let mut first = pool.acquire().unwrap();
            first
                .insert(
                    2,
                    buffer(ranges.capture(std::slice::from_ref(&(0..16))).unwrap()),
                )
                .unwrap();
            let second = first.clone();
            let barrier = Barrier::new(2);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    barrier.wait();
                    drop(second);
                });
                barrier.wait();
                drop(first);
                // Acquisition may race either last return's Weak probe. It
                // must either refuse or obtain already-cleared unique backing.
                if let Ok(reclaimed) = pool.acquire() {
                    assert!(reclaimed.is_empty());
                }
            });
            assert!(pool.is_idle());
            assert!(ranges.is_idle());
        }
    }

    #[test]
    fn submissions_remain_readable_when_the_pool_is_destroyed_first() {
        let mut pool = ExportPool::new(1, 1).unwrap();
        let mut first = pool.acquire().unwrap();
        first.insert(3, image(42)).unwrap();
        let retained = first.clone();
        drop(pool);
        drop(first);
        assert!(matches!(retained.get(3), Some(Export::Image(value)) if value.revision == 42));
    }
}

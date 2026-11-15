//! Prepared immutable import-range snapshots, independently retained by tokens.

use std::{
    fmt,
    ops::{Deref, Range},
    sync::Arc,
};

use super::{GraphError, GraphErrorKind};

struct RangeSlot {
    ranges: Vec<Range<u64>>,
}

/// An immutable snapshot of initialized ranges. Cloning retains the prepared
/// slot without allocating. Tokens can outlive the pool and remain readable.
#[derive(Clone)]
pub struct ImportRanges(Arc<RangeSlot>);

impl ImportRanges {
    /// Correctness-fixture preparation, excluded from normal frame APIs.
    #[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
    pub(crate) fn test_capture(ranges: &[Range<u64>]) -> Self {
        ImportRangesPool::new(ranges.len(), 1)
            .unwrap()
            .capture(ranges)
            .unwrap()
    }
    pub fn as_slice(&self) -> &[Range<u64>] {
        &self.0.ranges
    }
}

impl Deref for ImportRanges {
    type Target = [Range<u64>];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl fmt::Debug for ImportRanges {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ImportRanges")
            .field(&self.as_slice())
            .finish()
    }
}

impl PartialEq for ImportRanges {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}
impl Eq for ImportRanges {}

/// Storage reserved at an explicit preparation boundary. Capture and token
/// return do not grow or release any allocation while the pool remains alive.
/// A slot is writable only after every token and clone for it has been dropped.
pub struct ImportRangesPool {
    slots: Vec<Arc<RangeSlot>>,
    range_capacity: usize,
}

impl ImportRangesPool {
    pub fn new(range_capacity: usize, slots: usize) -> Result<Self, GraphError> {
        if slots == 0 {
            return Err(capacity("import range pool needs at least one slot"));
        }
        let mut prepared = Vec::new();
        prepared
            .try_reserve_exact(slots)
            .map_err(|_| capacity("import range slot reservation failed"))?;
        for _ in 0..slots {
            let mut ranges = Vec::new();
            ranges
                .try_reserve_exact(range_capacity)
                .map_err(|_| capacity("import range reservation failed"))?;
            prepared.push(Arc::new(RangeSlot { ranges }));
        }
        Ok(Self {
            slots: prepared,
            range_capacity,
        })
    }

    /// Copies into an unused prepared slot, then freezes it until all returned
    /// tokens are dropped. Contents are preserved exactly; import construction
    /// separately establishes resource bounds and initialized-range validity.
    pub fn capture(&mut self, ranges: &[Range<u64>]) -> Result<ImportRanges, GraphError> {
        if ranges.len() > self.range_capacity {
            return Err(capacity("initialized ranges exceed prepared capacity"));
        }
        for slot in &mut self.slots {
            if let Some(storage) = Arc::get_mut(slot) {
                storage.ranges.clear();
                storage.ranges.extend_from_slice(ranges);
                return Ok(ImportRanges(Arc::clone(slot)));
            }
        }
        Err(capacity("all prepared import range slots are retained"))
    }

    /// Observes how many slots currently have tokens. Concurrent token drops
    /// can change this snapshot; use [`Self::is_idle`] before replacing a pool.
    pub fn leased_slots(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| Arc::strong_count(slot) != 1)
            .count()
    }

    /// Checks unique ownership of every slot using `Arc::get_mut`, including
    /// its synchronization with token release. No capture can run concurrently
    /// through this exclusive pool borrow, and slot Arcs/Weak handles do not
    /// escape. A successful check permits replacing the pool before capturing
    /// another token; `leased_slots() == 0` alone is only an observation.
    pub fn is_idle(&mut self) -> bool {
        self.slots
            .iter_mut()
            .all(|slot| Arc::get_mut(slot).is_some())
    }
}

impl fmt::Debug for ImportRangesPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportRangesPool")
            .field("slots", &self.slots.len())
            .field("range_capacity", &self.range_capacity)
            .finish()
    }
}

fn capacity(message: &'static str) -> GraphError {
    GraphError::new(GraphErrorKind::CapacityExhausted, message)
}

#[cfg(test)]
#[allow(
    clippy::single_range_in_vec_init,
    reason = "Typed fixtures intentionally capture a single initialized byte interval."
)]
mod tests {
    use super::*;

    #[test]
    fn clones_keep_a_slot_frozen_and_return_it_for_reuse() {
        let mut pool = ImportRangesPool::new(2, 1).unwrap();
        let token = pool.capture(&[0..4, 8..12]).unwrap();
        let retained = token.clone();
        assert_eq!(pool.leased_slots(), 1);
        assert!(!pool.is_idle());
        assert_eq!(&*retained, &[0..4, 8..12]);
        drop(token);
        assert_eq!(
            pool.capture(&[4..8]).unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
        drop(retained);
        assert_eq!(pool.leased_slots(), 0);
        assert!(pool.is_idle());
        let reused = pool.capture(&[4..8]).unwrap();
        assert_eq!(reused.as_slice(), &[4..8]);
        assert_eq!(
            pool.capture(&[]).unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
    }

    #[test]
    fn capacity_errors_and_empty_snapshots_preserve_existing_tokens() {
        assert_eq!(
            ImportRangesPool::new(1, 0).unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
        let mut pool = ImportRangesPool::new(1, 2).unwrap();
        let token = pool.capture(&[0..4]).unwrap();
        assert_eq!(
            pool.capture(&[0..4, 8..12]).unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
        assert_eq!(token.as_slice(), &[0..4]);
        let empty = pool.capture(&[]).unwrap();
        assert!(empty.is_empty());
        assert_ne!(token, empty);
        drop(empty);
        let equal = pool.capture(&[0..4]).unwrap();
        assert_eq!(token, equal);
        let mut zero_ranges = ImportRangesPool::new(0, 1).unwrap();
        assert!(zero_ranges.capture(&[]).unwrap().is_empty());
        assert_eq!(
            zero_ranges.capture(&[0..1]).unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
    }

    #[test]
    fn token_transfer_and_pool_shutdown_preserve_immutable_contents() {
        let mut pool = ImportRangesPool::new(2, 1).unwrap();
        let token = pool.capture(&[12..16]).unwrap();
        let returned = std::thread::spawn(move || {
            assert_eq!(token.as_slice(), &[12..16]);
            token
        })
        .join()
        .unwrap();
        assert_eq!(
            pool.capture(&[]).unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
        drop(pool);
        assert_eq!(returned.as_slice(), &[12..16]);
        let copy = returned.clone();
        drop(returned);
        assert_eq!(copy.as_slice(), &[12..16]);
    }
}

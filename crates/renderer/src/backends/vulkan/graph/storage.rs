//! Prepared backing stays owned by the pool across checkout, retirement and
//! CPU result return. A normal lease Drop resets elements, never the allocation.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
};

use super::{RuntimeError, RuntimeErrorKind};

struct Owner<T> {
    idle: Mutex<Vec<T>>,
    capacity: usize,
    reset: fn(&mut T),
}

pub(super) struct StoragePool<T>(Arc<Owner<T>>);
impl<T> Clone for StoragePool<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}
impl<T> StoragePool<T> {
    pub(super) fn new(
        capacity: usize,
        mut create: impl FnMut() -> Result<T, RuntimeError>,
        reset: fn(&mut T),
    ) -> Result<Self, RuntimeError> {
        if capacity == 0 {
            return Err(RuntimeError::invalid(
                "prepared storage capacity must be nonzero",
            ));
        }
        let mut idle = Vec::new();
        idle.try_reserve_exact(capacity)
            .map_err(|error| RuntimeError::with_source(RuntimeErrorKind::Native, error))?;
        for _ in 0..capacity {
            idle.push(create()?);
        }
        Ok(Self(Arc::new(Owner {
            idle: Mutex::new(idle),
            capacity,
            reset,
        })))
    }

    pub(super) fn acquire(&self) -> Result<StorageLease<T>, RuntimeError> {
        let data = self
            .0
            .idle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop()
            .ok_or_else(|| {
                RuntimeError::capacity_with(
                    "prepared frame storage exhausted",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::FrameStorage,
                        CapacityRecovery::ReleaseOwners,
                        Some(self.0.capacity as u64),
                        None,
                    ),
                )
            })?;
        Ok(StorageLease {
            data: Some(data),
            owner: Arc::clone(&self.0),
        })
    }

    pub(super) fn is_idle(&self) -> bool {
        self.0
            .idle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
            == self.0.capacity
    }
}

pub(super) struct StorageLease<T> {
    data: Option<T>,
    owner: Arc<Owner<T>>,
}
impl<T> Deref for StorageLease<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.data.as_ref().expect("live prepared storage lease")
    }
}
impl<T> DerefMut for StorageLease<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.data.as_mut().expect("live prepared storage lease")
    }
}
impl<T> Drop for StorageLease<T> {
    fn drop(&mut self) {
        let Some(mut data) = self.data.take() else {
            return;
        };
        // Reset may return other leases and take their locks. It runs before
        // taking this pool lock, avoiding a nested pool-lock order.
        (self.owner.reset)(&mut data);
        let mut idle = self
            .owner
            .idle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(
            idle.len() < self.owner.capacity,
            "unique prepared storage checkout"
        );
        idle.push(data);
    }
}

pub(super) fn reserved<T>(capacity: usize) -> Result<Vec<T>, RuntimeError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|error| RuntimeError::with_source(RuntimeErrorKind::Native, error))?;
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_ownership_keeps_a_slot_until_return_without_replacing_backing() {
        let pool = StoragePool::new(2, || reserved::<u64>(16), Vec::clear).unwrap();
        let mut first = pool.acquire().unwrap();
        let pointer = first.as_ptr();
        first.extend(0..16);
        let second = pool.acquire().unwrap();
        let error = pool.acquire().err().unwrap();
        assert_eq!(
            error.capacity(),
            Some(RuntimeCapacityError::new(
                RuntimeCapacityKind::FrameStorage,
                CapacityRecovery::ReleaseOwners,
                Some(2),
                None
            ))
        );
        drop(first);
        let reused = pool.acquire().unwrap();
        assert_eq!(reused.as_ptr(), pointer);
        assert!(reused.is_empty());
        assert!(!pool.is_idle());
        drop((reused, second));
        assert!(pool.is_idle());
    }
}

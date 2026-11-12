//! Bounded native execution slots. Only proven-retired leases return to the pool.

use std::ops::Deref;
use std::sync::{Arc, Mutex, MutexGuard};

use super::{
    ExecutionPool, ExecutionPoolStats, ExecutionReservation, MemoryOwner, NativeTransfer,
    RecordedTransfer, TransferError, TransferPayload,
};
use crate::backends::vulkan::instance::LOG_TARGET;

/// Idle native owners retain MemoryOwner, never this state. A checked-out lease
/// separately retains the state, so neither normal recycling nor forgetting an
/// unresolved submit can destroy native dependencies or form an Arc cycle.
pub(super) struct PoolState {
    owner: Arc<MemoryOwner>,
    slots: Mutex<Slots<NativeTransfer>>,
}

struct Slots<T> {
    capacity: usize,
    allocated: usize,
    idle: Vec<T>,
    native_creates: usize,
    reuses: usize,
    resets: usize,
    capacity_exhaustions: usize,
}

impl<T> Slots<T> {
    fn new(capacity: usize) -> Result<Self, TransferError> {
        if capacity == 0 {
            return Err(TransferError::InvalidPool("capacity must be nonzero"));
        }
        Ok(Self {
            capacity,
            allocated: 0,
            idle: Vec::new(),
            native_creates: 0,
            reuses: 0,
            resets: 0,
            capacity_exhaustions: 0,
        })
    }

    /// None reserves room for creation; Some returns an already-reset slot.
    fn acquire(&mut self) -> Result<Option<T>, TransferError> {
        if let Some(slot) = self.idle.pop() {
            self.reuses = self.reuses.saturating_add(1);
            return Ok(Some(slot));
        }
        if self.allocated == self.capacity {
            self.capacity_exhaustions = self.capacity_exhaustions.saturating_add(1);
            return Err(TransferError::CapacityExhausted {
                capacity: self.capacity,
            });
        }
        self.allocated += 1;
        Ok(None)
    }

    fn release(&mut self, slot: T) {
        self.resets = self.resets.saturating_add(1);
        self.idle.push(slot);
    }

    fn discard(&mut self) {
        self.allocated -= 1;
    }

    fn stats(&self) -> ExecutionPoolStats {
        ExecutionPoolStats {
            capacity: self.capacity,
            allocated: self.allocated,
            leased: self.allocated - self.idle.len(),
            idle: self.idle.len(),
            native_creates: self.native_creates,
            reuses: self.reuses,
            resets: self.resets,
            capacity_exhaustions: self.capacity_exhaustions,
        }
    }
}

impl PoolState {
    fn lock(&self) -> MutexGuard<'_, Slots<NativeTransfer>> {
        match self.slots.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // There is no external code under this lock. If an invariant
                // nonetheless unwinds, retire the Device rather than reuse
                // possibly interrupted bookkeeping. Cleanup can still proceed.
                self.owner.device.stop();
                self.slots.clear_poison();
                poisoned.into_inner()
            }
        }
    }

    fn acquire(self: &Arc<Self>) -> Result<NativeLease, TransferError> {
        self.owner
            .device
            .ensure_active()
            .map_err(TransferError::DeviceUnavailable)?;
        let existing = {
            let mut slots = self.lock();
            self.owner
                .device
                .ensure_active()
                .map_err(TransferError::DeviceUnavailable)?;
            slots.acquire()?
        };
        // Establish the reservation before any native creation or logging that
        // can fail/unwind. A failed new slot releases its reserved capacity.
        let mut lease = NativeLease {
            native: existing,
            recycler: Some(Arc::clone(self)),
        };
        if lease.native.is_none() {
            lease.native = Some(NativeTransfer::new(Arc::clone(&self.owner))?);
            let mut slots = self.lock();
            slots.native_creates = slots.native_creates.saturating_add(1);
        }
        Ok(lease)
    }
}

impl ExecutionPool {
    /// The limit includes idle slots, reservations, and in-flight submissions.
    /// Slots are created lazily and acquisition never waits for GPU completion.
    pub(super) fn new(owner: Arc<MemoryOwner>, max_slots: usize) -> Result<Self, TransferError> {
        let slots = Slots::new(max_slots)?;
        owner
            .device
            .ensure_active()
            .map_err(TransferError::DeviceUnavailable)?;
        Ok(Self {
            state: Arc::new(PoolState {
                owner,
                slots: Mutex::new(slots),
            }),
        })
    }

    /// Reserve before accepting caller data when capacity failure must leave
    /// those inputs untouched. Dropping the reservation returns the idle slot.
    pub(super) fn reserve(&self) -> Result<ExecutionReservation, TransferError> {
        let result = self
            .state
            .acquire()
            .map(|native| ExecutionReservation { native });
        if let Err(error @ TransferError::Vulkan { .. }) = &result {
            tracing::error!(target: LOG_TARGET, error = %error,
                "Vulkan execution slot creation failed");
        }
        result
    }

    /// Convenience for already-owned payloads; errors consume the payload.
    pub(super) fn record<T: TransferPayload>(
        &self,
        payload: T,
    ) -> Result<RecordedTransfer<T>, TransferError> {
        payload.validate()?;
        self.reserve()?.record(payload)
    }

    pub(super) fn stats(&self) -> ExecutionPoolStats {
        self.state.lock().stats()
    }
}

impl ExecutionReservation {
    pub(super) fn record<T: TransferPayload>(
        self,
        payload: T,
    ) -> Result<RecordedTransfer<T>, TransferError> {
        let result = (|| {
            payload.validate()?;
            if !Arc::ptr_eq(payload.owner(), &self.native.owner) {
                return Err(TransferError::InvalidPool(
                    "payload must belong to the pool's Allocator and Device",
                ));
            }
            self.native
                .owner
                .device
                .ensure_active()
                .map_err(TransferError::DeviceUnavailable)?;
            RecordedTransfer::with_native(payload, self.native)
        })();
        if let Err(error @ TransferError::Vulkan { .. }) = &result {
            tracing::error!(target: LOG_TARGET, error = %error,
                "Vulkan pooled command recording failed");
        }
        result
    }
}

/// This wrapper drops before the payload. Retained only drops it after cancel,
/// actual completion or Device loss; unresolved pending work leaks it intact.
pub(super) struct NativeLease {
    native: Option<NativeTransfer>,
    recycler: Option<Arc<PoolState>>,
}

impl NativeLease {
    #[cfg(test)]
    pub(super) fn standalone(native: NativeTransfer) -> Self {
        Self {
            native: Some(native),
            recycler: None,
        }
    }
}

impl Deref for NativeLease {
    type Target = NativeTransfer;

    fn deref(&self) -> &Self::Target {
        self.native.as_ref().expect("live execution lease")
    }
}

impl Drop for NativeLease {
    fn drop(&mut self) {
        let Some(recycler) = self.recycler.take() else {
            return; // standalone native owner is destroyed by normal field Drop
        };
        let Some(native) = self.native.take() else {
            recycler.lock().discard(); // native creation failed or unwound
            return;
        };
        if native.owner.device.ensure_active().is_err() {
            recycler.lock().discard();
            drop(native);
            return;
        }
        let result = native.reset();
        {
            let mut slots = recycler.lock();
            if result.is_ok() && native.owner.device.ensure_active().is_ok() {
                slots.release(native);
                return;
            }
            slots.discard();
        }
        // No GPU work remains. Failed reset retires this slot rather than
        // returning an invalid recording state to another caller.
        drop(native);
        if let Err(error) = result {
            tracing::error!(target: LOG_TARGET, error = %error,
                "Vulkan execution slot reset failed; slot retired");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_capacity_counts_leases_idle_reuse_and_explicit_release() {
        assert!(matches!(
            Slots::<usize>::new(0),
            Err(TransferError::InvalidPool(_))
        ));
        let mut slots = Slots::<usize>::new(2).unwrap();
        assert_eq!(slots.acquire().unwrap(), None);
        assert_eq!(slots.acquire().unwrap(), None);
        assert!(matches!(
            slots.acquire(),
            Err(TransferError::CapacityExhausted { capacity: 2 })
        ));
        assert_eq!(slots.stats().leased, 2);
        assert_eq!(slots.stats().capacity_exhaustions, 1);
        slots.release(17);
        assert_eq!(slots.stats().idle, 1);
        assert_eq!(slots.stats().leased, 1);
        assert_eq!(slots.acquire().unwrap(), Some(17));
        assert_eq!(slots.stats().reuses, 1);
        assert_eq!(slots.stats().resets, 1);
        assert_eq!(slots.stats().allocated, 2);
        slots.discard();
        assert_eq!(slots.stats().allocated, 1);
        assert_eq!(slots.acquire().unwrap(), None);
        assert_eq!(slots.stats().leased, 2);
    }

    #[test]
    fn failed_creation_or_reset_releases_capacity_instead_of_recycling() {
        let mut slots = Slots::<usize>::new(1).unwrap();
        assert_eq!(slots.acquire().unwrap(), None);
        slots.discard();
        assert_eq!(slots.stats().allocated, 0);
        assert_eq!(slots.stats().idle, 0);
        assert_eq!(slots.acquire().unwrap(), None);
        slots.release(23);
        assert_eq!(slots.acquire().unwrap(), Some(23));
        slots.discard();
        assert_eq!(slots.stats().allocated, 0);
        assert_eq!(slots.stats().resets, 1);
    }

    #[test]
    #[ignore = "explicit real-GPU execution reuse test; requires validation and SyncVal"]
    fn vulkan_execution_pool_smoke() {
        use std::time::Duration;

        use ash::vk;

        use crate::backends::vulkan::buffer::BufferRole;
        use crate::backends::vulkan::device::VulkanDevice;
        use crate::backends::vulkan::diagnostics::Diagnostics;
        use crate::backends::vulkan::instance::VulkanInstance;
        use crate::backends::vulkan::memory::VulkanAllocator;
        use crate::backends::vulkan::transfer::TransferBuffers;

        fn payload(allocator: &VulkanAllocator, byte: u8) -> TransferBuffers {
            let mut upload = allocator.create_buffer(BufferRole::Upload, 257).unwrap();
            upload.write(0, &[byte; 257]).unwrap();
            TransferBuffers {
                upload,
                transfer: allocator
                    .create_buffer(BufferRole::DeviceTransfer, 257)
                    .unwrap(),
                readback: allocator.create_buffer(BufferRole::Readback, 257).unwrap(),
            }
        }

        let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
        tracing::dispatcher::with_default(&dispatch, || {
            let diagnostics = Diagnostics::new();
            let instance = VulkanInstance::new(Arc::clone(&diagnostics)).unwrap();
            crate::backends::vulkan::assert_real_validation(&diagnostics);
            let device = VulkanDevice::new(instance).unwrap();
            assert_ne!(device.device_type(), vk::PhysicalDeviceType::CPU);
            let allocator = VulkanAllocator::new(device).unwrap();
            let lifetime = Arc::downgrade(&allocator.owner);
            let pool = ExecutionPool::new(Arc::clone(&allocator.owner), 2).unwrap();

            // Cancel both bare reservations and an actual native recording.
            // Resetting the pool must precede release of its referenced buffers.
            drop(pool.reserve().unwrap());
            drop(pool.record(payload(&allocator, 0x13)).unwrap());
            assert_eq!(pool.stats().native_creates, 1);
            assert_eq!(pool.stats().idle, 1);

            let mut first = pool
                .record(payload(&allocator, 0x37))
                .unwrap()
                .submit()
                .unwrap();
            let mut second = pool
                .record(payload(&allocator, 0x59))
                .unwrap()
                .submit()
                .unwrap();
            // A driver may already be finished, but until explicit completion
            // observation both retained leases still occupy their slots.
            assert!(matches!(
                pool.reserve(),
                Err(TransferError::CapacityExhausted { capacity: 2 })
            ));
            assert_eq!(pool.stats().leased, 2);
            assert_eq!(pool.stats().capacity_exhaustions, 1);

            for (pending, expected) in [(&mut first, 0x37), (&mut second, 0x59)] {
                let mut completed = pending
                    .wait(Duration::from_secs(10))
                    .unwrap()
                    .expect("tiny copy must finish within the test budget");
                let mut output = [0; 257];
                completed.readback.read(0, &mut output).unwrap();
                assert_eq!(output, [expected; 257]);
            }
            assert_eq!(pool.stats().native_creates, 2);
            assert_eq!(pool.stats().idle, 2);
            assert_eq!(pool.stats().leased, 0);
            assert!(pool.stats().reuses >= 2);
            assert_eq!(pool.stats().resets, 4);

            let pending = pool
                .record(payload(&allocator, 0x7b))
                .unwrap()
                .submit()
                .unwrap();
            assert_eq!(pool.stats().native_creates, 2);
            // A pending lease keeps pool state, idle slots, and native ancestors
            // alive even when the public pool/factory values are destroyed.
            drop(pool);
            drop(allocator);
            assert!(lifetime.upgrade().is_some());
            drop(pending);
            assert!(lifetime.upgrade().is_none());
            assert!(diagnostics.snapshot().is_clean());
        });
    }
}

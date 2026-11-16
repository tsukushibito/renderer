//! Optional timestamps retained by the same owner as the presentation commands.

use super::*;
use std::collections::VecDeque;

pub(super) struct ProfileStorage {
    pub(super) timestamps: TimestampPool,
    completed: VecDeque<PresentationMetrics>,
    capacity: usize,
}

impl ProfileStorage {
    pub(super) fn new(owner: Arc<MemoryOwner>, capacity: usize) -> Result<Self, RuntimeError> {
        let mut completed = VecDeque::new();
        completed.try_reserve_exact(capacity).map_err(|_| {
            RuntimeError::capacity("cannot reserve completed presentation timing samples")
        })?;
        Ok(Self {
            timestamps: TimestampPool::new(owner, capacity)?,
            completed,
            capacity,
        })
    }

    pub(super) fn reserve(&self, pending: usize) -> Result<TimestampLease, RuntimeError> {
        // Pending records and copied results share one bound. Query acquisition
        // cannot grow the completed queue after an accepted native submission.
        if self.completed.len() + pending >= self.capacity {
            return Err(RuntimeError::capacity(
                "presentation timing capacity exhausted; consume completed samples",
            ));
        }
        self.timestamps.acquire()
    }

    pub(super) fn push(&mut self, sample: PresentationMetrics) {
        // Every accepted profile reserved against this combined limit before
        // acquire. No mutation can increase pending+completed behind that guard.
        assert!(self.completed.len() < self.capacity);
        self.completed.push_back(sample);
    }

    pub(super) fn take(&mut self) -> Option<PresentationMetrics> {
        self.completed.pop_front()
    }
}

pub(super) struct PresentProfile {
    pub(super) lease: TimestampLease,
    started: Instant,
    pub(super) retire_started: Option<Instant>,
}

impl PresentProfile {
    pub(super) fn new(lease: TimestampLease, started: Instant) -> Self {
        Self {
            lease,
            started,
            retire_started: None,
        }
    }

    /// # Safety
    /// The successful Fence wait for this payload established native completion.
    pub(super) unsafe fn finish(&self) -> Result<GpuTiming, RuntimeError> {
        // SAFETY: The caller proves completion of the command that reset and
        // wrote this unique query pair. Retrieval adds no GPU wait.
        unsafe { self.lease.read(self.started.elapsed()) }
    }
}

pub(super) struct PendingPresent {
    pub(super) work: PendingTransfer<PresentWork>,
    pub(super) metrics: Option<PresentationMetrics>,
}

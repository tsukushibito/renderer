use std::time::{Duration, Instant};

pub(crate) enum Observation<R, E> {
    Pending,
    Complete(R),
    Failed { error: E, retired: bool },
}

/// The probe is the only external completion boundary. Keeping this collection
/// independent of a driver lets ownership/error transitions be tested on CPU.
pub(crate) struct FrameQueue<T> {
    entries: Vec<T>,
    limit: usize,
}

impl<T> FrameQueue<T> {
    pub(crate) fn new(limit: usize) -> Result<Self, std::collections::TryReserveError> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(limit)?;
        Ok(Self { entries, limit })
    }

    #[cfg(feature = "vulkan")]
    pub(crate) fn set_limit(&mut self, limit: usize) -> bool {
        if limit == 0 || limit > self.entries.capacity() || !self.entries.is_empty() {
            return false;
        }
        self.limit = limit;
        true
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_full(&self) -> bool {
        self.len() == self.limit
    }

    pub(crate) fn push(&mut self, value: T) {
        // Capacity is checked before accepting GPU work, and the pre-reserved
        // collection is uniquely borrowed for the whole submit transaction.
        assert!(!self.is_full(), "frame capacity reserved before submit");
        self.entries.push(value);
    }

    fn observe<R, E>(
        &mut self,
        mut probe: impl FnMut(&mut T) -> Observation<R, E>,
    ) -> Result<Option<R>, E> {
        for index in 0..self.entries.len() {
            if let Some(result) = self.probe_at(index, &mut probe)? {
                return Ok(Some(result));
            }
        }
        Ok(None)
    }

    /// Observe only the selected entry, leaving other completion results available.
    pub(crate) fn wait_for<R, E>(
        &mut self,
        matches: impl FnMut(&T) -> bool,
        probe: impl FnOnce(&mut T) -> Observation<R, E>,
        unknown: impl FnOnce() -> E,
    ) -> Result<Option<R>, E> {
        let index = self.entries.iter().position(matches).ok_or_else(unknown)?;
        self.probe_at(index, probe)
    }

    fn probe_at<R, E>(
        &mut self,
        index: usize,
        probe: impl FnOnce(&mut T) -> Observation<R, E>,
    ) -> Result<Option<R>, E> {
        match probe(&mut self.entries[index]) {
            Observation::Pending => Ok(None),
            Observation::Complete(result) => {
                self.entries.remove(index);
                Ok(Some(result))
            }
            Observation::Failed { error, retired } => {
                if retired {
                    self.entries.remove(index);
                }
                Err(error)
            }
        }
    }

    pub(crate) fn wait_one<R, E>(
        &mut self,
        timeout: Duration,
        mut probe: impl FnMut(&mut T, Duration) -> Observation<R, E>,
    ) -> Result<Option<R>, E> {
        let start = Instant::now();
        // Prefer any result already available, including a later frame, before
        // blocking on the oldest pending submission from this single queue.
        let completed = self.observe(|frame| probe(frame, Duration::ZERO))?;
        if completed.is_some() || self.entries.is_empty() {
            return Ok(completed);
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Ok(None);
        }
        // Only one native wait receives the remaining budget. Waiting on every
        // pending frame with the original timeout would multiply the API wait.
        self.probe_at(0, |frame| probe(frame, remaining))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;
    use std::rc::Rc;

    struct Work(Rc<Cell<usize>>);

    impl Drop for Work {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn bounded_queue_retains_unresolved_work_and_releases_only_retired_entries() {
        let drops = Rc::new(Cell::new(0));
        let mut frames = FrameQueue::new(2).unwrap();
        frames.push(Work(Rc::clone(&drops)));
        frames.push(Work(Rc::clone(&drops)));
        assert!(frames.is_full());
        assert_eq!(
            frames.observe::<(), &str>(|_| Observation::Pending),
            Ok(None)
        );
        assert_eq!(drops.get(), 0);
        assert_eq!(
            frames.observe::<(), _>(|_| Observation::Failed {
                error: "completion unproven",
                retired: false,
            }),
            Err("completion unproven")
        );
        assert_eq!(frames.len(), 2);
        assert_eq!(drops.get(), 0);
        assert_eq!(
            frames.observe::<_, &str>(|_| Observation::Complete(17)),
            Ok(Some(17))
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(drops.get(), 1);
        frames.push(Work(Rc::clone(&drops)));
        assert_eq!(
            frames.observe::<(), _>(|_| Observation::Failed {
                error: "device lost; no valid output",
                retired: true,
            }),
            Err("device lost; no valid output")
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(drops.get(), 2);
        drop(frames);
        assert_eq!(drops.get(), 3);
    }

    #[test]
    fn pending_front_does_not_hide_an_independently_observed_completion() {
        let mut frames = FrameQueue::new(2).unwrap();
        frames.push(false);
        frames.push(true);
        assert_eq!(
            frames.wait_one::<_, ()>(Duration::from_secs(1), |ready, budget| {
                assert!(budget.is_zero(), "ready results must avoid blocking");
                if *ready {
                    Observation::Complete(23)
                } else {
                    Observation::Pending
                }
            }),
            Ok(Some(23))
        );
        assert_eq!(frames.entries, [false]);
    }

    #[test]
    fn zero_timeout_polls_all_frames_and_timeout_waits_only_the_oldest() {
        let mut frames = FrameQueue::new(2).unwrap();
        frames.push(10);
        frames.push(20);
        let mut probes = Vec::new();
        assert_eq!(
            frames.wait_one::<(), ()>(Duration::ZERO, |frame, budget| {
                probes.push((*frame, budget));
                Observation::Pending
            }),
            Ok(None)
        );
        assert_eq!(probes, [(10, Duration::ZERO), (20, Duration::ZERO)]);
        assert!(frames.is_full());

        probes.clear();
        let timeout = Duration::from_secs(1);
        assert_eq!(
            frames.wait_one::<(), ()>(timeout, |frame, budget| {
                probes.push((*frame, budget));
                Observation::Pending
            }),
            Ok(None)
        );
        assert_eq!(probes.len(), 3);
        assert_eq!(probes[..2], [(10, Duration::ZERO), (20, Duration::ZERO)]);
        assert_eq!(probes[2].0, 10);
        assert!(!probes[2].1.is_zero());
        assert!(probes[2].1 <= timeout);
        assert_eq!(frames.entries, [10, 20], "timeout retains all ownership");
    }

    #[test]
    fn blocking_wait_returns_one_result_and_preserves_unproven_errors() {
        let mut frames = FrameQueue::new(2).unwrap();
        frames.push(10);
        frames.push(20);
        let timeout = Duration::from_secs(1);
        assert_eq!(
            frames.wait_one::<(), _>(timeout, |_, budget| {
                if budget.is_zero() {
                    Observation::Pending
                } else {
                    Observation::Failed {
                        error: "completion unproven",
                        retired: false,
                    }
                }
            }),
            Err("completion unproven")
        );
        assert_eq!(frames.entries, [10, 20]);
        assert_eq!(
            frames.wait_one::<_, ()>(timeout, |frame, budget| {
                if budget.is_zero() {
                    Observation::Pending
                } else {
                    Observation::Complete(*frame + 1)
                }
            }),
            Ok(Some(11))
        );
        assert_eq!(frames.entries, [20]);
        assert_eq!(
            frames.wait_one::<(), _>(timeout, |_, budget| {
                if budget.is_zero() {
                    Observation::Pending
                } else {
                    Observation::Failed {
                        error: "retired without valid output",
                        retired: true,
                    }
                }
            }),
            Err("retired without valid output")
        );
        assert!(frames.entries.is_empty());
        assert_eq!(
            frames.wait_one::<(), ()>(timeout, |_, _| panic!("empty queue must not wait")),
            Ok(None)
        );
    }

    #[test]
    fn targeted_wait_leaves_other_completions_available() {
        let mut frames = FrameQueue::new(3).unwrap();
        frames.push(5);
        frames.push(11);
        frames.push(17);

        assert_eq!(
            frames.wait_for(
                |&id| id == 11,
                |&mut id| {
                    assert_eq!(id, 11);
                    Observation::<(), &str>::Pending
                },
                || "unknown frame",
            ),
            Ok(None)
        );
        assert_eq!(
            frames.wait_for(
                |&id| id == 11,
                |&mut id| Observation::Complete(id),
                || "unknown frame"
            ),
            Ok(Some(11))
        );
        assert_eq!(
            frames.wait_for(
                |&id| id == 11,
                |_| -> Observation<(), _> { panic!("missing frame must not be probed") },
                || "unknown frame",
            ),
            Err("unknown frame")
        );
        assert_eq!(frames.entries, [5, 17]);
    }
}

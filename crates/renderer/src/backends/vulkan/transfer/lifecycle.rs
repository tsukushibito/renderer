//! Ownership transitions at the submit/wait boundary, independent of a driver.

use ash::vk;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Recorded,
    Pending,
    Complete,
    DeviceLost,
}

/// Keeps the resource owner intact until completion or a result that permits
/// retirement. In particular, an error is not generally evidence of completion.
pub(super) struct Retained<T> {
    value: Option<T>,
    state: State,
}

impl<T> Retained<T> {
    pub(super) fn new(value: T) -> Self {
        Self {
            value: Some(value),
            state: State::Recorded,
        }
    }

    pub(super) fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// Must precede the native call so unwinding cannot retire uncertain work.
    pub(super) fn begin_submit(&mut self) {
        assert_eq!(self.state, State::Recorded);
        self.state = State::Pending;
    }

    pub(super) fn submitted(&mut self, result: Result<(), vk::Result>) -> Result<(), vk::Result> {
        assert_eq!(self.state, State::Pending);
        match result {
            Ok(()) => {}
            // vkQueueSubmit2 guarantees these failures leave referenced objects
            // unchanged. An unrecognized error has no such completion proof.
            Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY) => {
                self.state = State::Recorded;
            }
            Err(vk::Result::ERROR_DEVICE_LOST) => self.state = State::DeviceLost,
            Err(_) => {}
        }
        result
    }

    pub(super) fn waited(&mut self, result: Result<(), vk::Result>) -> Result<bool, vk::Result> {
        assert_eq!(self.state, State::Pending);
        match result {
            Ok(()) => {
                self.state = State::Complete;
                Ok(true)
            }
            Err(vk::Result::TIMEOUT) => Ok(false),
            Err(vk::Result::ERROR_DEVICE_LOST) => {
                // Lost-device rules permit retirement of pending resources, but
                // do not establish valid contents or successful execution.
                self.state = State::DeviceLost;
                Err(vk::Result::ERROR_DEVICE_LOST)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn is_pending(&self) -> bool {
        self.state == State::Pending
    }

    pub(super) fn is_lost(&self) -> bool {
        self.state == State::DeviceLost
    }

    pub(super) fn is_complete(&self) -> bool {
        self.state == State::Complete
    }

    pub(super) fn take_retired(&mut self) -> Option<T> {
        if self.is_pending() {
            None
        } else {
            self.value.take()
        }
    }
}

impl<T> Drop for Retained<T> {
    fn drop(&mut self) {
        if self.is_pending()
            && let Some(value) = self.value.take()
        {
            // The outer owner normally attempts bounded cleanup and records an
            // unresolved failure. This also preserves native dependencies if an
            // unwind interrupts that path before it can complete.
            std::mem::forget(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct Resource(Rc<Cell<usize>>);

    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn unsubmitted_and_unchanged_rejected_work_can_be_retired() {
        let drops = Rc::new(Cell::new(0));
        drop(Retained::new(Resource(Rc::clone(&drops))));
        assert_eq!(drops.get(), 1);
        for error in [
            vk::Result::ERROR_OUT_OF_HOST_MEMORY,
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        ] {
            let mut retained = Retained::new(Resource(Rc::clone(&drops)));
            retained.begin_submit();
            assert_eq!(retained.submitted(Err(error)), Err(error));
            assert!(!retained.is_complete());
            assert!(retained.take_retired().is_some());
        }
        assert_eq!(drops.get(), 3);
    }

    #[test]
    fn timeout_and_wait_failure_retain_resources_until_actual_completion() {
        let drops = Rc::new(Cell::new(0));
        let mut retained = Retained::new(Resource(Rc::clone(&drops)));
        retained.begin_submit();
        retained.submitted(Ok(())).unwrap();
        assert!(!retained.waited(Err(vk::Result::TIMEOUT)).unwrap());
        assert!(retained.take_retired().is_none());
        assert_eq!(
            retained.waited(Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY)),
            Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY)
        );
        assert!(retained.take_retired().is_none());
        assert_eq!(drops.get(), 0);
        assert!(retained.waited(Ok(())).unwrap());
        assert!(retained.is_complete());
        let resource = retained
            .take_retired()
            .expect("completion releases ownership");
        assert!(retained.value().is_none());
        drop(retained);
        assert_eq!(drops.get(), 0);
        drop(resource);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn lost_device_permits_cleanup_without_reporting_success() {
        let drops = Rc::new(Cell::new(0));
        let mut retained = Retained::new(Resource(Rc::clone(&drops)));
        retained.begin_submit();
        retained.submitted(Ok(())).unwrap();
        assert_eq!(
            retained.waited(Err(vk::Result::ERROR_DEVICE_LOST)),
            Err(vk::Result::ERROR_DEVICE_LOST)
        );
        assert!(retained.is_lost());
        assert!(!retained.is_complete());
        assert!(retained.take_retired().is_some());
        assert_eq!(drops.get(), 1);

        let mut rejected = Retained::new(Resource(Rc::clone(&drops)));
        rejected.begin_submit();
        assert_eq!(
            rejected.submitted(Err(vk::Result::ERROR_DEVICE_LOST)),
            Err(vk::Result::ERROR_DEVICE_LOST)
        );
        assert!(rejected.is_lost());
        assert!(!rejected.is_complete());
        drop(rejected);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn unresolved_submission_is_retained_even_when_owner_unwinds() {
        let drops = Rc::new(Cell::new(0));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut retained = Retained::new(Resource(Rc::clone(&drops)));
            retained.begin_submit();
            assert_eq!(
                retained.submitted(Err(vk::Result::ERROR_UNKNOWN)),
                Err(vk::Result::ERROR_UNKNOWN)
            );
            assert!(retained.is_pending());
            assert!(retained.take_retired().is_none());
            panic!("simulated host unwind");
        }));
        assert!(result.is_err());
        assert_eq!(drops.get(), 0);
        // One intentionally retained CPU-only sentinel models native ownership;
        // no Vulkan object or allocation is created by this test.
        assert_eq!(Rc::strong_count(&drops), 2);
    }
}

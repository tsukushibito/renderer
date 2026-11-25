//! Bounded queue of borrowed right branches. The queue mutex orders publication,
//! unique removal, and completion with respect to the submitting stack frame.
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

pub(crate) struct Scheduler {
    state: Mutex<State>,
    available: Condvar,
    progress: Condvar,
    pub(crate) helpers: usize,
    #[cfg(test)]
    pub(crate) publications: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) wake_requests: std::sync::atomic::AtomicUsize,
}

struct State {
    queue: Vec<Task>,
    idle: usize,
    ready: usize,
    joining: usize,
    shutdown: bool,
}

struct Task {
    callback: *mut (),
    invoke: unsafe fn(*mut ()),
    completed: *const AtomicBool,
}

// SAFETY: fork publishes only Send callbacks. The frame and its completion
// flag remain alive until execution and acknowledgement under the mutex.
unsafe impl Send for Task {}

// No internal unwind may retire a stack frame that is still published.
struct Published;
impl Drop for Published {
    fn drop(&mut self) {
        std::process::abort();
    }
}

impl Scheduler {
    pub(crate) fn new(helpers: usize) -> std::io::Result<Self> {
        let mut queue = Vec::new();
        queue
            .try_reserve_exact(helpers)
            .map_err(std::io::Error::other)?;
        Ok(Self {
            state: Mutex::new(State {
                queue,
                idle: 0,
                ready: 0,
                joining: 0,
                shutdown: false,
            }),
            available: Condvar::new(),
            progress: Condvar::new(),
            helpers,
            #[cfg(test)]
            publications: Default::default(),
            #[cfg(test)]
            wake_requests: Default::default(),
        })
    }

    pub(crate) fn stop(&self) {
        self.state.lock().unwrap().shutdown = true;
        self.available.notify_all();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn wait_ready(&self) {
        let state = self.state.lock().unwrap();
        drop(
            self.progress
                .wait_while(state, |s| s.ready < self.helpers)
                .unwrap(),
        );
    }

    pub(crate) fn worker(&self) {
        let mut state = self.state.lock().unwrap();
        state.ready += 1;
        if state.ready == self.helpers {
            self.progress.notify_all();
        }
        loop {
            if let Some(task) = state.queue.pop() {
                drop(state);
                self.execute(task);
                state = self.state.lock().unwrap();
            } else if state.shutdown {
                return;
            } else {
                state.idle += 1;
                state = self.available.wait(state).unwrap();
                state.idle -= 1;
            }
        }
    }

    fn execute(&self, task: Task) {
        // SAFETY: removal gives exclusive execution. The callback catches the
        // user's unwind before returning; it is never accessed after this call.
        unsafe { (task.invoke)(task.callback) };
        let state = self.state.lock().unwrap();
        // SAFETY: join waits for this write before it may discard the flag.
        unsafe { &*task.completed }.store(true, Ordering::Relaxed);
        if state.joining != 0 {
            self.progress.notify_all();
        }
    }

    pub(crate) fn fork<A, B>(
        &self,
        left: impl FnOnce() -> A,
        right: impl FnOnce() -> B + Send,
    ) -> (A, B)
    where
        B: Send,
    {
        let done = AtomicBool::new(false);
        let mut result = None;
        let mut right = Some(right);
        let mut call = || result = Some(catch_unwind(AssertUnwindSafe(right.take().unwrap())));
        unsafe fn invoke<F: FnMut()>(p: *mut ()) {
            // SAFETY: erased by task_for using this exact type.
            unsafe { (&mut *p.cast::<F>())() };
        }
        fn task_for<F: FnMut() + Send>(call: &mut F, done: &AtomicBool) -> Task {
            Task {
                callback: std::ptr::from_mut(call).cast(),
                invoke: invoke::<F>,
                completed: done,
            }
        }
        let mut state = self.state.lock().unwrap();
        if state.queue.len() == self.helpers {
            drop(state);
            let a = catch_unwind(AssertUnwindSafe(left));
            call();
            return (
                a.unwrap_or_else(|p| resume_unwind(p)),
                result.unwrap().unwrap_or_else(|p| resume_unwind(p)),
            );
        }
        let guard = Published;
        state.queue.push(task_for(&mut call, &done));
        #[cfg(test)]
        self.publications.fetch_add(1, Ordering::Relaxed);
        if state.idle != 0 {
            #[cfg(test)]
            self.wake_requests.fetch_add(1, Ordering::Relaxed);
            self.available.notify_one();
        } else if state.joining != 0 {
            self.progress.notify_one();
        }
        drop(state);
        let a = catch_unwind(AssertUnwindSafe(left));
        let mut state = self.state.lock().unwrap();
        while !done.load(Ordering::Relaxed) {
            let own = state
                .queue
                .iter()
                .rposition(|task| std::ptr::eq(task.completed, &done));
            let task = match own {
                Some(i) => Some(state.queue.swap_remove(i)),
                None => state.queue.pop(),
            };
            if let Some(task) = task {
                drop(state);
                self.execute(task);
                state = self.state.lock().unwrap();
            } else {
                state.joining += 1;
                state = self.progress.wait(state).unwrap();
                state.joining -= 1;
            }
        }
        drop(state);
        std::mem::forget(guard);
        (
            a.unwrap_or_else(|p| resume_unwind(p)),
            result.unwrap().unwrap_or_else(|p| resume_unwind(p)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_queue_executes_nested_pair_locally() {
        // Leave the single slot occupied throughout the nested fork.
        let scheduler = Scheduler::new(1).unwrap();
        let pair = scheduler.fork(|| scheduler.fork(|| 1, || 2), || 3);
        assert_eq!(pair, ((1, 2), 3));
        assert_eq!(scheduler.publications.load(Ordering::Relaxed), 1);
    }
}

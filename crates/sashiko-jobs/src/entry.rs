//! Reusable host-to-worker handoff. Rayon's external injector rotates heap
//! blocks even with one outstanding job. Keep one prepared slot for the host.
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, Condvar, Mutex};

pub(super) struct Entry(Arc<Shared>);

#[derive(Default)]
struct Shared {
    sender: Mutex<()>,
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    ready: bool,
    job: Option<BorrowedCall>,
    completed: bool,
    visitors: usize,
    shutdown: bool,
}

struct BorrowedCall {
    data: *mut (),
    call: unsafe fn(*mut ()),
}

// Even an unexpected synchronization panic must not unwind the borrowed
// stack frame while its address is visible to the receiver.
struct Published;
impl Drop for Published {
    fn drop(&mut self) {
        std::process::abort();
    }
}

// SAFETY: new only accepts Send callbacks. The submitting stack frame waits
// for completion under the same mutex before the callback can be accessed again.
unsafe impl Send for BorrowedCall {}

impl BorrowedCall {
    // SAFETY: the sender must keep the callback exclusively borrowed and live
    // until the receiver signals completion. The callback must not unwind.
    unsafe fn new<F: FnMut() + Send>(f: &mut F) -> Self {
        unsafe fn invoke<F: FnMut()>(ptr: *mut ()) {
            // SAFETY: the constructor erases this exact F. The single receiver
            // consumes the request while the submitting frame is waiting.
            unsafe { (&mut *ptr.cast::<F>())() };
        }
        Self {
            data: std::ptr::from_mut(f).cast(),
            call: invoke::<F>,
        }
    }
}

impl Entry {
    pub(super) fn new() -> Self {
        Self(Arc::default())
    }

    pub(super) fn service(&self) -> impl FnOnce() + Send + 'static {
        let shared = self.0.clone();
        move || {
            let mut state = shared.state.lock().unwrap();
            state.ready = true;
            shared.wake.notify_all();
            loop {
                if let Some(job) = state.job.take() {
                    drop(state);
                    // SAFETY: run publishes a live, non-unwinding callback and
                    // waits for completed. No user callback runs under the lock.
                    unsafe { (job.call)(job.data) };
                    state = shared.state.lock().unwrap();
                    state.completed = true;
                    shared.wake.notify_all();
                } else if state.visitors != 0 {
                    // Another producer is entering this pool through Rayon.
                    // Also service that queue on a one-worker pool.
                    drop(state);
                    let idle = rayon::yield_now() != Some(rayon::Yield::Executed);
                    state = shared.state.lock().unwrap();
                    if idle && state.visitors != 0 && state.job.is_none() {
                        state = shared
                            .wake
                            .wait_timeout(state, std::time::Duration::from_micros(100))
                            .unwrap()
                            .0;
                    }
                } else if state.shutdown {
                    return;
                } else {
                    state = shared.wake.wait(state).unwrap();
                }
            }
        }
    }

    pub(super) fn wait_ready(&self) {
        let state = self.0.state.lock().unwrap();
        drop(self.0.wake.wait_while(state, |state| !state.ready).unwrap());
    }

    pub(super) fn run<F, R>(&self, work: F, install: impl FnOnce(F) -> R) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        // A worker entering from a different pool must use Rayon's cooperative
        // wait. Concurrent external producers also keep making progress there.
        let sender = if rayon::current_thread_index().is_none() {
            self.0.sender.try_lock().ok()
        } else {
            None
        };
        let Some(sender) = sender else {
            let visitor = Visitor::enter(&self.0);
            let result = install(work);
            drop(visitor);
            return result;
        };

        let mut work = Some(work);
        let mut result = None;
        let mut callback = || result = Some(catch_unwind(AssertUnwindSafe(work.take().unwrap())));
        let mut state = self.0.state.lock().unwrap();
        state.completed = false;
        let published = Published;
        // SAFETY: sender serializes slot reuse. From publication until
        // completion this frame only waits; the lock is private and no user
        // code runs while holding it. The result (including a panic) is read
        // only after reacquiring the completion mutex.
        state.job = Some(unsafe { BorrowedCall::new(&mut callback) });
        self.0.wake.notify_one();
        while !state.completed {
            state = self.0.wake.wait(state).unwrap();
        }
        std::mem::forget(published);
        drop(state);
        drop(sender);
        result
            .unwrap()
            .unwrap_or_else(|payload| resume_unwind(payload))
    }
}

struct Visitor<'a>(&'a Shared);

impl<'a> Visitor<'a> {
    fn enter(shared: &'a Shared) -> Self {
        shared.state.lock().unwrap().visitors += 1;
        shared.wake.notify_all();
        Self(shared)
    }
}

impl Drop for Visitor<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().visitors -= 1;
        self.0.wake.notify_all();
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().shutdown = true;
        self.0.wake.notify_all();
    }
}

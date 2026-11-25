//! Scoped CPU jobs. Independent submissions can run in any order, including
//! immediately on the submitting thread. Never block on another scoped job.
//!
//! ```
//! use sashiko_jobs::JobSystem;
//! let mut jobs = JobSystem::inline();
//! let (mut count, mut poses) = (0, [1; 16]);
//! jobs.scope(|s| {
//!     s.spawn(|| count = 42);
//!     s.for_each_mut(&mut poses, |pose| *pose += 1);
//! });
//! assert_eq!((count, poses), (42, [2; 16]));
//! ```
#![deny(unsafe_code)]

use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Mutex;
#[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
use std::sync::atomic::AtomicUsize;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
#[allow(unsafe_code)]
mod scheduler;
#[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
mod web;

/// Own this at the host boundary. Parallelism includes the calling thread.
pub struct JobSystem {
    #[cfg(not(target_arch = "wasm32"))]
    workers: Option<native::Workers>,
    #[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
    workers: Option<web::Workers>,
}

/// Cumulative activity since team creation. Snapshots may overlap concurrent work.
#[cfg(feature = "scheduler-metrics")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerCounts {
    pub published: usize,
    pub stolen: usize,
    pub empty_victim_probes: usize,
    pub work_wake_requests: usize,
}

impl JobSystem {
    /// Execute on the caller alone, without allocating or creating workers.
    pub const fn inline() -> Self {
        Self {
            #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
            workers: None,
        }
    }

    /// Create a native team with parallelism - 1 persistent helpers.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(parallelism: NonZeroUsize) -> std::io::Result<Self> {
        if parallelism == NonZeroUsize::MIN {
            return Ok(Self::inline());
        }
        Ok(Self {
            workers: Some(native::Workers::new(parallelism.get() - 1)?),
        })
    }

    /// Maximum number of simultaneous participants, including the host.
    pub fn parallelism(&self) -> NonZeroUsize {
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        if let Some(w) = &self.workers {
            return NonZeroUsize::new(w.scheduler.helpers + 1).unwrap();
        }
        NonZeroUsize::MIN
    }

    /// Cumulative publications, successful steals, empty victim probes, and work wake requests.
    #[cfg(feature = "scheduler-metrics")]
    pub fn scheduler_counts(&self) -> SchedulerCounts {
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        if let Some(team) = &self.workers {
            let [published, stolen, empty_victim_probes, work_wake_requests] = team
                .scheduler
                .counters
                .each_ref()
                .map(|c| c.load(std::sync::atomic::Ordering::Relaxed));
            return SchedulerCounts {
                published,
                stolen,
                empty_victim_probes,
                work_wake_requests,
            };
        }
        SchedulerCounts::default()
    }
    /// Run the body on this host, then join all submissions and descendants.
    /// The first recorded job panic propagates after drain; a body panic takes precedence.
    pub fn scope<'env, R>(
        &'env mut self,
        body: impl for<'s> FnOnce(&'s JobScope<'s, 'env>) -> R,
    ) -> R {
        scoped(
            #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
            self.workers.as_ref().map(|w| &*w.scheduler),
            body,
        )
    }
}

struct Group {
    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
    pending: AtomicUsize,
    panic: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}
impl Group {
    fn capture(&self, job: impl FnOnce()) {
        if let Err(payload) = catch_unwind(AssertUnwindSafe(job)) {
            let mut first = self.panic.lock().unwrap();
            if first.is_none() {
                *first = Some(payload);
            } else {
                drop(first);
                // A panicking payload destructor must not kill a worker with borrowed work outstanding.
                if catch_unwind(AssertUnwindSafe(|| drop(payload))).is_err() {
                    std::process::abort();
                }
            }
        }
    }
}

fn scoped<'env, R>(
    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))] scheduler: Option<
        &'env scheduler::Scheduler,
    >,
    body: impl for<'s> FnOnce(&'s JobScope<'s, 'env>) -> R,
) -> R {
    let scope = JobScope {
        group: Group {
            #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
            pending: AtomicUsize::new(0),
            panic: Mutex::new(None),
        },
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        scheduler,
        lifetimes: PhantomData,
    };
    let result = catch_unwind(AssertUnwindSafe(|| body(&scope)));
    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
    if let Some(scheduler) = scheduler {
        // A failed internal wait must never unwind past outstanding borrows.
        if catch_unwind(AssertUnwindSafe(|| scheduler.drain(&scope.group))).is_err() {
            std::process::abort();
        }
    }
    let output = result.unwrap_or_else(|p| resume_unwind(p));
    if let Some(p) = scope.group.panic.lock().unwrap().take() {
        resume_unwind(p);
    }
    output
}

/// Borrowed region. Invariance prevents shortening the scope lifetime to
/// publish a closure borrowing a body-local variable.
pub struct JobScope<'s, 'env: 's> {
    group: Group,
    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
    scheduler: Option<&'env scheduler::Scheduler>,
    lifetimes: PhantomData<&'s mut &'s &'env ()>,
}

impl<'s, 'env: 's> JobScope<'s, 'env> {
    /// Maximum simultaneous participants in this team, including its host.
    pub fn parallelism(&self) -> NonZeroUsize {
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        if let Some(pool) = self.scheduler {
            return NonZeroUsize::new(pool.helpers + 1).unwrap();
        }
        NonZeroUsize::MIN
    }
    /// Join a subgroup before consuming its results. Outer siblings remain independent.
    pub fn scope<'a, R>(
        &'a self,
        body: impl for<'child> FnOnce(&'child JobScope<'child, 'a>) -> R,
    ) -> R {
        scoped(
            #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
            self.scheduler,
            body,
        )
    }
    /// Submit an independent job. Capture this scope to submit descendants.
    #[allow(unsafe_code)]
    /// ```compile_fail
    /// use sashiko_jobs::JobSystem;
    /// JobSystem::inline().scope(|s| {
    ///     let local = String::from("body-local");
    ///     s.spawn(|| println!("{local}"));
    /// });
    /// ```
    pub fn spawn(&self, job: impl FnOnce() + Send + 's) {
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        let job = if let Some(pool) = self.scheduler {
            // SAFETY: scoped drains even after a panic. All captures outlive 's.
            match unsafe { pool.publish(&self.group, job) } {
                Ok(()) => return,
                Err(job) => job,
            }
        } else {
            job
        };
        self.group.capture(job);
    }

    /// Schedule disjoint elements. All visits have finished on scope exit.
    pub fn for_each_mut<T: Send + 's>(
        &self,
        items: &'s mut [T],
        visit: impl Fn(&mut T) + Send + Sync + 's,
    ) {
        self.for_each_chunk_mut(items, NonZeroUsize::MIN, move |_, chunk| {
            visit(&mut chunk[0])
        });
    }

    /// Keep logical chunk boundaries and starting indices independent of scheduling.
    pub fn for_each_chunk_mut<T: Send + 's>(
        &self,
        items: &'s mut [T],
        size: NonZeroUsize,
        visit: impl Fn(usize, &mut [T]) + Send + Sync + 's,
    ) {
        if items.is_empty() {
            return;
        }
        let size = size.get();
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        if let Some(pool) = self.scheduler.filter(|_| items.len() > size) {
            self.spawn(move || {
                scoped(Some(pool), |children| {
                    let chunks = items.len().div_ceil(size);
                    let width = chunks.div_ceil(pool.helpers + 1).saturating_mul(size);
                    let mut ranges = items.chunks_mut(width).enumerate();
                    // Keep one range on the participant that is already running.
                    let (last, local) = ranges.next_back().unwrap();
                    for (i, range) in ranges {
                        submit_range(children, pool, i * width, range, size, &visit);
                    }
                    visit_range(last * width, local, size, &visit);
                })
            });
            return;
        }
        self.group.capture(|| visit_range(0, items, size, &visit));
    }
}

fn visit_range<T>(start: usize, items: &mut [T], size: usize, visit: &impl Fn(usize, &mut [T])) {
    for (i, chunk) in items.chunks_mut(size).enumerate() {
        visit(start + i * size, chunk);
    }
}

#[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
fn submit_range<'s, T: Send, F: Fn(usize, &mut [T]) + Sync>(
    scope: &'s JobScope<'s, '_>,
    pool: &'s scheduler::Scheduler,
    start: usize,
    items: &'s mut [T],
    size: usize,
    visit: &'s F,
) {
    let owner = pool.lane();
    scope.spawn(move || {
        let count = items.len().div_ceil(size);
        if count > 1 && owner != pool.lane() && pool.needs_split(owner) {
            let split = (count / 2) * size;
            let (left, right) = items.split_at_mut(split);
            submit_range(scope, pool, start + split, right, size, visit);
            visit_range(start, left, size, visit);
        } else {
            visit_range(start, items, size, visit);
        }
    });
}
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn saturated_queue_and_overaligned_captures_run_inline() {
        use std::sync::atomic::Ordering::Relaxed;
        let pool = scheduler::Scheduler::new(1).unwrap();
        let visits = AtomicUsize::new(0);
        // No helper is launched: queued work cannot be stolen before drain.
        scoped(Some(&pool), |s| {
            for _ in 0..256 {
                s.spawn(|| {
                    visits.fetch_add(1, Relaxed);
                });
            }
            assert!((1..256).contains(&visits.load(Relaxed)));
        });
        assert_eq!(visits.load(Relaxed), 256);
        #[repr(align(256))]
        struct Aligned(u8);
        let input = Aligned(21);
        let output = AtomicUsize::new(0);
        let dest = &output;
        scoped(Some(&pool), |s| {
            s.spawn(move || dest.store(std::hint::black_box(input).0.into(), Relaxed));
            assert_eq!(dest.load(Relaxed), 21);
        });
    }
}

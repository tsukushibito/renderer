//! Host-participating, scoped CPU parallelism. A run begins on its caller.
//! Branches must be independent: blocking on a sibling or on I/O may prevent progress.
//! ```
//! use sashiko_jobs::JobSystem;
//! let mut system = JobSystem::inline();
//! let mut poses = [0; 5];
//! let result = system.run(|jobs| jobs.join(
//!     |jobs| jobs.for_each_chunk_mut(&mut poses, 2.try_into().unwrap(), |start, out| out.fill(start)),
//!     |_| 5,
//! ));
//! assert_eq!(result.1, poses.len());
//! assert_eq!(poses, [0, 0, 2, 2, 4]);
//! ```
#![deny(unsafe_code)]

use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

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

    /// Run a borrowed region on this caller. Even a non-Send host closure or
    /// result is allowed. The context itself cannot escape the region.
    pub fn run<R>(&mut self, work: impl FnOnce(&JobContext<'_>) -> R) -> R {
        work(&JobContext {
            #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
            scheduler: self.workers.as_ref().map(|w| &*w.scheduler),
            local: std::marker::PhantomData,
        })
    }
}

/// Local capability for nested fork/join. Each branch receives its own context.
pub struct JobContext<'a> {
    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
    scheduler: Option<&'a scheduler::Scheduler>,
    local: std::marker::PhantomData<&'a std::rc::Rc<()>>,
}

impl JobContext<'_> {
    /// The left branch runs here. The right branch may be stolen. Both settle
    /// before returning or propagating an unwind, with results in argument order.
    pub fn join<A, B: Send>(
        &self,
        left: impl FnOnce(&Self) -> A,
        right: impl FnOnce(&Self) -> B + Send,
    ) -> (A, B) {
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        if let Some(scheduler) = self.scheduler {
            return scheduler.fork(
                || left(self),
                || {
                    right(&Self {
                        scheduler: Some(scheduler),
                        local: std::marker::PhantomData,
                    })
                },
            );
        }
        let a = catch_unwind(AssertUnwindSafe(|| left(self)));
        let b = catch_unwind(AssertUnwindSafe(|| right(self)));
        (
            a.unwrap_or_else(|p| resume_unwind(p)),
            b.unwrap_or_else(|p| resume_unwind(p)),
        )
    }

    /// Visit each disjoint logical chunk exactly once on normal return. Its
    /// starting index and boundaries do not depend on parallelism.
    pub fn for_each_chunk_mut<T: Send>(
        &self,
        slice: &mut [T],
        chunk_size: NonZeroUsize,
        visit: impl Fn(usize, &mut [T]) + Sync,
    ) {
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        if let Some(scheduler) = self.scheduler {
            return self.chunks(
                slice,
                chunk_size.get(),
                0,
                (scheduler.helpers + 1).saturating_mul(2),
                &visit,
            );
        }
        visit_chunks(slice, chunk_size.get(), 0, &visit);
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
    fn chunks<T: Send, F: Fn(usize, &mut [T]) + Sync>(
        &self,
        slice: &mut [T],
        size: usize,
        start: usize,
        budget: usize,
        visit: &F,
    ) {
        let chunks = slice.len().div_ceil(size);
        if budget <= 1 || chunks <= 1 {
            return visit_chunks(slice, size, start, visit);
        }
        let split = (chunks / 2) * size;
        let (a, b) = slice.split_at_mut(split);
        self.join(
            |ctx| ctx.chunks(a, size, start, budget / 2, visit),
            |ctx| ctx.chunks(b, size, start + split, budget - budget / 2, visit),
        );
    }
}
fn visit_chunks<T>(slice: &mut [T], size: usize, start: usize, visit: &impl Fn(usize, &mut [T])) {
    for (i, chunk) in slice.chunks_mut(size).enumerate() {
        visit(start + i * size, chunk);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn host_only_regions_issue_no_worker_wakes() {
        let mut system = JobSystem::new(4.try_into().unwrap()).unwrap();
        let mut data = [0; 4];
        for _ in 0..100 {
            system.run(|ctx| {
                ctx.for_each_chunk_mut(&mut data, 4.try_into().unwrap(), |_, v| v[0] += 1)
            });
        }
        assert_eq!(data[0], 100);
        let scheduler = &system.workers.as_ref().unwrap().scheduler;
        assert_eq!(
            scheduler
                .wake_requests
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(
            scheduler
                .publications
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
}

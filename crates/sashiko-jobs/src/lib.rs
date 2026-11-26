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

use std::num::NonZeroUsize;
mod backend;
mod scope;
use backend::Backend;
pub use scope::JobScope;
use scope::scoped;
mod shared;
pub use shared::SharedJobs;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
#[allow(unsafe_code)]
mod scheduler;
#[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
mod web;

/// Own this at the host boundary. Parallelism includes the calling thread.
pub struct JobSystem {
    backend: Backend,
}

/// Cumulative activity since team creation. Snapshots may overlap concurrent work.
#[cfg(feature = "scheduler-metrics")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerCounts {
    pub published: usize,
    pub stolen: usize,
    pub empty_victim_probes: usize,
    pub work_wake_requests: usize,
    /// Cumulative wall time spent parked while joining a scope (native only).
    /// Nested joins on different participants may overlap.
    #[cfg(not(target_arch = "wasm32"))]
    pub join_wait_ns: u64,
}

impl JobSystem {
    /// Execute on the caller alone, without allocating or creating workers.
    pub const fn inline() -> Self {
        Self {
            backend: Backend::Inline,
        }
    }

    /// Create a native team with parallelism - 1 persistent helpers.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(parallelism: NonZeroUsize) -> std::io::Result<Self> {
        if parallelism == NonZeroUsize::MIN {
            return Ok(Self::inline());
        }
        Ok(Self {
            backend: Backend::Workers(native::Workers::new(parallelism.get() - 1)?),
        })
    }

    /// Create an experimental Rayon team. Its scoped body runs on the caller.
    #[cfg(all(not(target_arch = "wasm32"), feature = "experimental-rayon"))]
    pub fn new_rayon(parallelism: NonZeroUsize) -> std::io::Result<Self> {
        let helpers = parallelism.get() - 1;
        if helpers == 0 {
            return Ok(Self::inline());
        }
        if helpers > rayon::max_num_threads() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "too many Rayon workers",
            ));
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(helpers)
            .build()
            .map_err(std::io::Error::other)?;
        Ok(Self {
            backend: Backend::Rayon(pool),
        })
    }

    /// Maximum number of simultaneous participants, including the host.
    pub fn parallelism(&self) -> NonZeroUsize {
        self.backend.team().parallelism()
    }

    /// Counters for the built-in worker scheduler. Inline and Rayon teams report zero.
    #[cfg(feature = "scheduler-metrics")]
    pub fn scheduler_counts(&self) -> SchedulerCounts {
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        if let Backend::Workers(team) = &self.backend {
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
                #[cfg(not(target_arch = "wasm32"))]
                join_wait_ns: team
                    .scheduler
                    .join_wait_ns
                    .load(std::sync::atomic::Ordering::Relaxed),
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
        scoped(self.backend.team(), body)
    }
}

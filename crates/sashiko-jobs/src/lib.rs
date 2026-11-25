//! Borrowed, structured CPU work on a host-selected executor.
//! Share a pool by reference. Tasks may run in any order and must be
//! independent: blocking on a sibling (or performing blocking I/O) can
//! prevent progress. Return values and disjoint output slices carry results.
//!
//! ```
//! use sashiko_jobs::JobPool;
//! use std::num::NonZeroUsize;
//!
//! let pool = JobPool::inline();
//! let mut positions = [0; 5];
//! let ((), count) = pool.join(
//!     || pool.for_each_chunk_mut(&mut positions, NonZeroUsize::new(2).unwrap(),
//!         |start, items| items.fill(start)),
//!     || 5,
//! );
//! assert_eq!(positions[..count], [0, 0, 2, 2, 4]);
//! ```
#![deny(unsafe_code)]

use std::num::NonZeroUsize;

#[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
#[allow(unsafe_code)]
mod entry;

#[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
mod web;

/// The active execution strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionMode {
    Inline,
    Parallel,
}

enum Backend {
    Inline,
    #[cfg(not(target_arch = "wasm32"))]
    Native(entry::Entry, rayon::ThreadPool),
    #[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
    Web(entry::Entry),
}

/// A shared executor. Borrowed work has finished when each operation returns.
pub struct JobPool(Backend);

impl JobPool {
    /// Run synchronously on the caller without creating workers.
    pub const fn inline() -> Self {
        Self(Backend::Inline)
    }

    /// Create a dedicated native pool. The global Rayon pool is never configured.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(workers: NonZeroUsize) -> std::io::Result<Self> {
        if workers.get() > rayon::max_num_threads() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "worker count exceeds pool limit",
            ));
        }
        rayon::ThreadPoolBuilder::new()
            .num_threads(workers.get())
            .thread_name(|i| format!("sashiko-job-{i}"))
            .build()
            .map(|pool| {
                let entry = entry::Entry::new();
                pool.spawn(entry.service());
                entry.wait_ready();
                Self(Backend::Native(entry, pool))
            })
            .map_err(std::io::Error::other)
    }

    pub fn mode(&self) -> ExecutionMode {
        match self.0 {
            Backend::Inline => ExecutionMode::Inline,
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Native(..) => ExecutionMode::Parallel,
            #[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
            Backend::Web(_) => ExecutionMode::Parallel,
        }
    }

    /// Fork two independent computations, returning results in argument order.
    /// In unwinding builds, both branches settle before a panic propagates.
    pub fn join<A: Send, B: Send>(
        &self,
        left: impl FnOnce() -> A + Send,
        right: impl FnOnce() -> B + Send,
    ) -> (A, B) {
        match &self.0 {
            Backend::Inline => {
                use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
                let a = catch_unwind(AssertUnwindSafe(left));
                let b = catch_unwind(AssertUnwindSafe(right));
                (
                    a.unwrap_or_else(|p| resume_unwind(p)),
                    b.unwrap_or_else(|p| resume_unwind(p)),
                )
            }
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Native(..) => self.parallel(|| rayon::join(left, right)),
            #[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
            Backend::Web(_) => self.parallel(|| rayon::join(left, right)),
        }
    }

    /// Visit each nonempty chunk exactly once on normal return. The callback
    /// receives its starting element index. Only the final chunk may be short.
    pub fn for_each_chunk_mut<T: Send>(
        &self,
        slice: &mut [T],
        chunk_size: NonZeroUsize,
        visit: impl Fn(usize, &mut [T]) + Sync,
    ) {
        let size = chunk_size.get();
        if slice.len() <= size || self.mode() == ExecutionMode::Inline {
            for (index, chunk) in slice.chunks_mut(size).enumerate() {
                visit(index * size, chunk);
            }
            return;
        }
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        {
            use rayon::prelude::*;
            self.parallel(|| {
                slice
                    .par_chunks_mut(size)
                    .enumerate()
                    .for_each(|(i, chunk)| visit(i * size, chunk));
            });
        }
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
    fn parallel<R: Send>(&self, work: impl FnOnce() -> R + Send) -> R {
        match &self.0 {
            Backend::Inline => unreachable!(),
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Native(entry, pool) => {
                if pool.current_thread_index().is_some() {
                    work()
                } else {
                    entry.run(work, |job| pool.install(job))
                }
            }
            #[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
            Backend::Web(entry) => {
                if rayon::current_thread_index().is_some() {
                    work()
                } else {
                    entry.run(work, |job| rayon::join(job, || ()).0)
                }
            }
        }
    }
}

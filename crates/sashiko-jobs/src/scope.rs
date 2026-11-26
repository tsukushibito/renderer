use super::backend::Team;
#[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
use super::scheduler;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Mutex;
#[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
use std::sync::atomic::AtomicUsize;

pub(super) struct Group {
    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
    pub(super) pending: AtomicUsize,
    panic: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}
impl Group {
    pub(super) fn capture(&self, job: impl FnOnce()) {
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

pub(super) fn scoped<'env, R>(
    team: Team<'env>,
    body: impl for<'s> FnOnce(&'s JobScope<'s, 'env>) -> R,
) -> R {
    let group = Group {
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        pending: AtomicUsize::new(0),
        panic: Mutex::new(None),
    };
    let scope = JobScope {
        region: Region {
            group: &group,
            team,
            lifetimes: PhantomData,
            #[cfg(all(
                feature = "experimental-rayon",
                any(not(target_arch = "wasm32"), feature = "web-threads")
            ))]
            rayon_scope: Default::default(),
        },
    };
    let run = || catch_unwind(AssertUnwindSafe(|| body(&scope)));
    let result = match team {
        #[cfg(all(
            feature = "experimental-rayon",
            any(not(target_arch = "wasm32"), feature = "web-threads")
        ))]
        Team::Rayon(pool) => pool.in_place_scope(|region| {
            scope.region.rayon_scope.store(
                std::ptr::from_ref(region).cast_mut(),
                std::sync::atomic::Ordering::Release,
            );
            run()
        }),
        _ => run(),
    };
    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
    if let Team::Workers(scheduler) = team
        && catch_unwind(AssertUnwindSafe(|| scheduler.drain(&group))).is_err()
    {
        // Outstanding borrowed callbacks must never escape on an internal join failure.
        std::process::abort();
    }
    let output = result.unwrap_or_else(|p| resume_unwind(p));
    if let Some(p) = group.panic.lock().unwrap().take() {
        resume_unwind(p);
    }
    output
}

/// Borrowed region. Invariance prevents shortening the scope lifetime to
/// publish a closure borrowing a body-local variable.
pub struct JobScope<'s, 'env: 's> {
    region: Region<'s, 'env>,
}

struct Region<'s, 'env: 's> {
    group: &'s Group,
    team: Team<'env>,
    #[cfg(all(
        feature = "experimental-rayon",
        any(not(target_arch = "wasm32"), feature = "web-threads")
    ))]
    rayon_scope: std::sync::atomic::AtomicPtr<rayon::Scope<'s>>,
    lifetimes: PhantomData<&'s mut &'s &'env ()>,
}

impl<'s, 'env: 's> JobScope<'s, 'env> {
    /// Maximum simultaneous participants in this team, including its host.
    pub fn parallelism(&self) -> NonZeroUsize {
        self.region.team.parallelism()
    }
    /// Join a subgroup before consuming its results. Outer siblings remain independent.
    pub fn scope<'a, R>(
        &'a self,
        body: impl for<'child> FnOnce(&'child JobScope<'child, 'a>) -> R,
    ) -> R {
        scoped(self.region.team, body)
    }
    /// Submit an independent job. Capture this scope to submit descendants.
    /// ```compile_fail
    /// use sashiko_jobs::JobSystem;
    /// JobSystem::inline().scope(|s| {
    ///     let local = String::from("body-local");
    ///     s.spawn(|| println!("{local}"));
    /// });
    /// ```
    pub fn spawn(&self, job: impl FnOnce() + Send + 's) {
        self.region.spawn(job);
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
        self.region.for_each_chunk_mut(items, size, visit);
    }
}

impl<'s, 'env: 's> Region<'s, 'env> {
    #[allow(unsafe_code)]
    fn spawn(&self, job: impl FnOnce() + Send + 's) {
        #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
        let job = if let Team::Workers(pool) = self.team {
            // SAFETY: scoped drains even after a panic. All captures outlive 's.
            match unsafe { pool.publish(self.group, job) } {
                Ok(()) => return,
                Err(job) => job,
            }
        } else {
            job
        };
        #[cfg(all(
            feature = "experimental-rayon",
            any(not(target_arch = "wasm32"), feature = "web-threads")
        ))]
        if let Team::Rayon(_) = self.team {
            let ptr = self.rayon_scope.load(std::sync::atomic::Ordering::Acquire);
            // SAFETY: scoped publishes this handle before exposing JobScope, and
            // keeps both JobScope and Group alive outside in_place_scope until
            // every descendant has returned. No caller can outlive that join.
            let region = unsafe { &*ptr };
            let group = self.group;
            region.spawn(move |_| group.capture(job));
            return;
        }
        self.group.capture(job);
    }

    fn for_each_chunk_mut<T: Send + 's>(
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
        if let Team::Workers(pool) = self.team
            && items.len() > size
        {
            self.spawn(move || parallel_ranges(pool, items, size, &visit));
            return;
        }
        #[cfg(all(
            feature = "experimental-rayon",
            any(not(target_arch = "wasm32"), feature = "web-threads")
        ))]
        if let Team::Rayon(_) = self.team
            && items.len() > size
        {
            self.spawn(move || {
                use rayon::prelude::*;
                items
                    .par_chunks_mut(size)
                    .enumerate()
                    .for_each(|(i, chunk)| visit(i * size, chunk));
            });
            return;
        }
        self.group.capture(|| visit_range(0, items, size, &visit));
    }
}

#[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
fn parallel_ranges<T: Send>(
    pool: &scheduler::Scheduler,
    items: &mut [T],
    size: usize,
    visit: &(impl Fn(usize, &mut [T]) + Sync),
) {
    let chunks = items.len().div_ceil(size);
    let participants = pool.helpers + 1;
    if pool.helpers > 1 && chunks / 4 >= participants {
        // Leave a second range per participant for uneven work, without publishing
        // a separate scheduler job for every range.
        let width = chunks.div_ceil(participants * 2).saturating_mul(size);
        let remaining = Mutex::new(items.chunks_mut(width).enumerate());
        let run = &|| loop {
            // Release the batch-local lock before invoking user code.
            let next = remaining.lock().unwrap().next();
            let Some((i, range)) = next else { break };
            visit_range(i * width, range, size, visit);
        };
        scoped(Team::Workers(pool), |workers| {
            for _ in 0..pool.helpers {
                workers.spawn(run);
            }
            run();
        });
    } else {
        let width = chunks.div_ceil(participants).saturating_mul(size);
        let mut ranges = items.chunks_mut(width).enumerate();
        let (last, local) = ranges.next_back().unwrap();
        scoped(Team::Workers(pool), |children| {
            for (i, range) in ranges {
                submit_range(children, pool, i * width, range, size, visit);
            }
            visit_range(last * width, local, size, visit);
        });
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
        scoped(Team::Workers(&pool), |s| {
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
        scoped(Team::Workers(&pool), |s| {
            s.spawn(move || dest.store(std::hint::black_box(input).0.into(), Relaxed));
            assert_eq!(dest.load(Relaxed), 21);
        });
    }
}

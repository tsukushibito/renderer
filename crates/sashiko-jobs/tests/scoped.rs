#![cfg(not(target_arch = "wasm32"))]

use sashiko_jobs::{ExecutionMode, JobPool};
use std::num::NonZeroUsize;

#[test]
fn borrowed_nested_subsystems_cover_each_chunk_once() {
    for count in [0, 1, 4] {
        let pool = NonZeroUsize::new(count)
            .map(|workers| JobPool::new(workers).unwrap())
            .unwrap_or_else(JobPool::inline);
        assert_eq!(
            pool.mode(),
            if count == 0 {
                ExecutionMode::Inline
            } else {
                ExecutionMode::Parallel
            }
        );
        for len in [0, 1, 7, 8, 9, 513] {
            let mut output = vec![0; len];
            let (tag, (a, b)) = pool.join(
                || {
                    pool.for_each_chunk_mut(
                        &mut output,
                        NonZeroUsize::new(8).unwrap(),
                        |start, chunk| {
                            assert_eq!(start % 8, 0);
                            assert_eq!(chunk.len(), (len - start).min(8));
                            for (offset, value) in chunk.iter_mut().enumerate() {
                                *value += start + offset + 1;
                            }
                        },
                    );
                    "done"
                },
                || pool.join(|| 17, || 23),
            );
            assert_eq!((tag, a, b), ("done", 17, 23));
            assert!(output.iter().copied().eq(1..=len));
        }
    }
}

#[test]
fn failed_branch_settles_borrowed_work_before_unwind() {
    let pool = JobPool::new(2.try_into().unwrap()).unwrap();
    let mut output = 0;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pool.join(|| panic!("failed"), || output = 42)
    }));
    assert!(panic.is_err());
    assert_eq!(output, 42);
    assert_eq!(pool.join(|| output, || 1), (42, 1));
}

#[test]
fn independent_hosts_and_nested_single_worker_pools_make_progress() {
    let a = JobPool::new(NonZeroUsize::MIN).unwrap();
    let b = JobPool::new(NonZeroUsize::MIN).unwrap();
    let start = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for seed in 0..4 {
            // Each producer enters the shared pool independently.
            let (a, b, start) = (&a, &b, &start);
            scope.spawn(move || {
                start.wait();
                for i in 0..100 {
                    let (value, ()) =
                        a.join(|| b.join(|| a.join(|| i + seed, || ()).0, || ()).0, || ());
                    assert_eq!(value, i + seed);
                }
            });
        }
    });
}

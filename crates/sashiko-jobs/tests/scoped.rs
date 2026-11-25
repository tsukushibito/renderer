#![cfg(not(target_arch = "wasm32"))]
use sashiko_jobs::JobSystem;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;

#[test]
fn descendants_chunks_and_scope_borrows() {
    for n in [1, 2, 4] {
        let mut jobs = JobSystem::new(n.try_into().unwrap()).unwrap();
        for len in [0, 1, 8, 13, 512, 515] {
            let mut a = vec![0; len];
            let mut b = vec![0; len];
            let count = AtomicUsize::new(0);
            jobs.scope(|s| {
                s.spawn(|| s.for_each_mut(&mut a, |v| *v += 1));
                s.for_each_chunk_mut(&mut b, 8.try_into().unwrap(), |start, chunk| {
                    assert_eq!(start % 8, 0);
                    assert_eq!(chunk.len(), 8.min(len - start));
                    for (i, v) in chunk.iter_mut().enumerate() {
                        *v += start + i + 1;
                    }
                });
                for _ in 0..80 {
                    s.spawn(|| {
                        count.fetch_add(1, Ordering::Relaxed);
                    });
                }
            });
            assert_eq!(count.into_inner(), 80);
            assert!(a.into_iter().all(|v| v == 1));
            assert!(b.into_iter().eq(1..=len));
        }
    }
}

#[test]
fn range_callback_panic_propagates_and_the_pool_can_run_another_batch() {
    let mut jobs = JobSystem::new(4.try_into().unwrap()).unwrap();
    let mut values = [0u8; 129];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        jobs.scope(|s| {
            s.for_each_chunk_mut(&mut values, 3.try_into().unwrap(), |start, _| {
                if start == 33 {
                    panic!("range callback");
                }
            })
        });
    }));
    assert_eq!(
        result.unwrap_err().downcast_ref::<&str>(),
        Some(&"range callback")
    );
    jobs.scope(|s| s.for_each_mut(&mut values, |v| *v += 1));
    assert_eq!(values, [1; 129]);
}
#[test]
fn unwind_drains_and_system_can_be_reused() {
    for n in [1, 4] {
        let mut jobs = JobSystem::new(n.try_into().unwrap()).unwrap();
        for body_panics in [true, false] {
            let mut value = 0;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                jobs.scope(|s| {
                    s.spawn(|| panic!("job panic"));
                    s.spawn(|| value = 42);
                    if body_panics {
                        panic!("body panic");
                    }
                })
            }));
            let payload = result.unwrap_err();
            assert_eq!(
                payload.downcast_ref::<&str>().copied(),
                Some(if body_panics {
                    "body panic"
                } else {
                    "job panic"
                })
            );
            assert_eq!(value, 42);
        }
    }
}

#[test]
fn subgroup_finishes_before_its_borrowed_result_is_used() {
    let mut jobs = JobSystem::new(3.try_into().unwrap()).unwrap();
    let host_value = std::rc::Rc::new(7);
    let mut value = 0;
    jobs.scope(|outer| {
        let input = *host_value;
        let output = &mut value;
        outer.spawn(move || {
            let mut local = [input; 16];
            outer.scope(|inner| inner.for_each_mut(&mut local, |v| *v *= 2));
            *output = local.iter().sum::<usize>();
        });
    });
    assert_eq!(value, 224);
}
#[test]
fn sleeping_helper_makes_progress() {
    let mut jobs = JobSystem::new(2.try_into().unwrap()).unwrap();
    let host = std::thread::current().id();
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(1));
        let (tx, rx) = mpsc::channel();
        jobs.scope(|s| {
            s.spawn(move || tx.send(std::thread::current().id()).unwrap());
            assert_ne!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), host);
        });
    }
}

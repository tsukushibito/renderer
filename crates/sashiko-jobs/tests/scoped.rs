#![cfg(not(target_arch = "wasm32"))]

use sashiko_jobs::JobSystem;
use std::num::NonZeroUsize;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn nested_clients_borrow_results_and_visit_stable_chunks() {
    for n in [1, 2, 4] {
        let mut system = JobSystem::new(n.try_into().unwrap()).unwrap();
        assert_eq!(system.parallelism().get(), n);
        for len in [0, 1, 7, 8, 9, 513] {
            let mut output = vec![0; len];
            let local = std::rc::Rc::new(7);
            let (tag, values) = system.run(|jobs| {
                jobs.join(
                    |jobs| jobs.join(|_| local.clone(), |_| {}).0,
                    |jobs| {
                        jobs.for_each_chunk_mut(
                            &mut output,
                            8.try_into().unwrap(),
                            |start, chunk| {
                                assert_eq!(start % 8, 0);
                                assert_eq!(chunk.len(), 8.min(len - start));
                                for (i, element) in chunk.iter_mut().enumerate() {
                                    *element += start + i + 1;
                                }
                            },
                        );
                        output.as_slice()
                    },
                )
            });
            assert_eq!(*tag, 7);
            assert!(values.iter().copied().eq(1..=len));
        }
    }
}

#[test]
fn both_branches_settle_before_unwind() {
    for n in [1, 2, 4] {
        let mut system = JobSystem::new(n.try_into().unwrap()).unwrap();
        for fail_left in [false, true] {
            let mut a = 0;
            let mut b = 0;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                system.run(|jobs| {
                    jobs.join(
                        |_| {
                            a = 3;
                            assert!(!fail_left);
                        },
                        |_| {
                            b = 4;
                            assert!(fail_left);
                        },
                    )
                })
            }));
            assert!(outcome.is_err());
            assert_eq!((a, b), (3, 4));
            assert_eq!(system.run(|jobs| jobs.join(|_| a, |_| b)), (3, 4));
        }
    }
}

#[test]
fn sleeping_worker_claims_an_independent_branch_without_further_submissions() {
    let host = thread::current().id();
    for _ in 0..3 {
        let mut system = JobSystem::new(2.try_into().unwrap()).unwrap();
        for _ in 0..8 {
            thread::sleep(Duration::from_millis(1));
            let (tx, rx) = mpsc::channel();
            system.run(|jobs| {
                jobs.join(
                    |_| {
                        assert_eq!(thread::current().id(), host);
                        rx.recv_timeout(Duration::from_secs(10)).unwrap();
                    },
                    move |_| {
                        assert_ne!(thread::current().id(), host);
                        tx.send(()).unwrap();
                    },
                )
            });
        }
    }
    let mut one = JobSystem::new(NonZeroUsize::MIN).unwrap();
    one.run(|jobs| {
        jobs.join(
            |_| assert_eq!(thread::current().id(), host),
            |_| assert_eq!(thread::current().id(), host),
        )
    });
}

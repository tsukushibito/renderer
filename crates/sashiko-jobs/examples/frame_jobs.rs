//! Isolated process-wide allocation and latency check of two borrowed clients.
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use sashiko_heap::{CountingAllocator, HeapSnapshot};
    use sashiko_jobs::{JobScope, JobSystem};
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    #[global_allocator]
    static HEAP: CountingAllocator<std::alloc::System> = CountingAllocator::new(std::alloc::System);

    fn client<'s>(pool: &JobScope<'s, '_>, data: &'s mut [u32]) {
        pool.for_each_chunk_mut(data, 256.try_into().unwrap(), |start, chunk| {
            for value in chunk {
                for _ in 0..64 {
                    *value = value.wrapping_mul(0x9e3779b1).rotate_left(11) ^ start as u32;
                }
            }
        });
    }

    for mut pool in [
        JobSystem::inline(),
        JobSystem::new(4.try_into().unwrap()).unwrap(),
    ] {
        for size in [32, 65536] {
            let mut values = vec![33; size];
            let mut times = [Duration::ZERO; 256];
            let mut frame = || {
                let (a, b) = black_box(&mut values).split_at_mut(size / 2);
                pool.scope(|jobs| {
                    client(jobs, a);
                    client(jobs, b);
                });
            };
            for _ in 0..128 {
                frame();
            }
            let before = HeapSnapshot::now();
            for time in &mut times {
                let start = Instant::now();
                frame();
                *time = start.elapsed();
            }
            let delta = HeapSnapshot::now().since(before);
            times.sort_unstable();
            println!(
                "{:?} n={size} p50={:?} p95={:?} alloc/zeroed/realloc/free={:?}",
                pool.parallelism(),
                times[127],
                times[243],
                delta.events.map(|entry| entry.calls)
            );
            assert!(delta.is_allocation_free(), "{delta:?}");
        }
        for rest in [false, true] {
            for split in [false, true] {
                let mut values = [1u32; 64];
                let mut times = [Duration::ZERO; 128];
                for time in &mut times {
                    if rest {
                        std::thread::sleep(Duration::from_millis(16));
                    }
                    let start = Instant::now();
                    pool.scope(|jobs| {
                        let visit = |start: usize, chunk: &mut [u32]| {
                            for value in chunk {
                                *value = black_box(*value).wrapping_add(start as u32 + 1);
                            }
                        };
                        if split {
                            jobs.for_each_chunk_mut(&mut values, 16.try_into().unwrap(), visit);
                        } else {
                            for (i, chunk) in values.chunks_mut(16).enumerate() {
                                visit(i * 16, chunk);
                            }
                        }
                    });
                    *time = start.elapsed();
                }
                black_box(values);
                times.sort_unstable();
                println!(
                    "parallelism={} idle={rest} split={split} p50={:?} p95={:?}",
                    pool.parallelism(),
                    times[63],
                    times[121]
                );
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}

//! Isolated process-wide allocation and latency check of two borrowed clients.
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use sashiko_jobs::JobPool;
    use sashiko_renderer_core::measurement::{CountingAllocator, Snapshot};
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    #[global_allocator]
    static HEAP: CountingAllocator<std::alloc::System> = CountingAllocator::new(std::alloc::System);

    fn client(pool: &JobPool, data: &mut [u32]) {
        pool.for_each_chunk_mut(data, 256.try_into().unwrap(), |start, chunk| {
            for value in chunk {
                for _ in 0..64 {
                    *value = value.wrapping_mul(0x9e3779b1).rotate_left(11) ^ start as u32;
                }
            }
        });
    }

    for pool in [
        JobPool::inline(),
        JobPool::new(4.try_into().unwrap()).unwrap(),
    ] {
        for size in [32, 65536] {
            let mut values = vec![33; size];
            let mut times = [Duration::ZERO; 256];
            let mut frame = || {
                let (a, b) = black_box(&mut values).split_at_mut(size / 2);
                pool.join(|| client(&pool, a), || client(&pool, b));
            };
            for _ in 0..128 {
                frame();
            }
            let before = Snapshot::now();
            for time in &mut times {
                let start = Instant::now();
                frame();
                *time = start.elapsed();
            }
            let delta = Snapshot::now().since(before);
            times.sort_unstable();
            println!(
                "{:?} n={size} p50={:?} p95={:?} alloc/zeroed/realloc/free={:?}",
                pool.mode(),
                times[127],
                times[243],
                delta.rust.map(|entry| entry.calls)
            );
            assert!(delta.is_allocation_free(), "{delta:?}");
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}

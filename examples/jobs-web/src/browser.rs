use sashiko_heap::{CountingAllocator, HeapSnapshot};
use sashiko_jobs::{JobScope, JobSystem};
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::thread::{self, ThreadId};
use wasm_bindgen::prelude::*;

#[global_allocator]
static HEAP: CountingAllocator<std::alloc::System> = CountingAllocator::new(std::alloc::System);

#[wasm_bindgen]
pub struct Example {
    pool: JobSystem,
}

#[wasm_bindgen]
impl Example {
    #[wasm_bindgen(js_name = start)]
    pub async fn start(parallelism: usize) -> Result<Example, JsValue> {
        let n = NonZeroUsize::new(parallelism).ok_or("parallelism must be positive")?;
        let pool = match n.get() {
            1 => JobSystem::inline(),
            #[cfg(feature = "web-threads")]
            _ => JobSystem::start_web(n).await?,
            #[cfg(not(feature = "web-threads"))]
            _ => return Err("this build supports inline execution".into()),
        };
        Ok(Self { pool })
    }

    pub fn parallel(&self) -> bool {
        self.pool.parallelism().get() > 1
    }

    /// All worker instances share Wasm memory, including these Rust counters.
    /// Allocate the JS result only after taking the final snapshot.
    pub fn frame_heap(&mut self) -> Vec<u32> {
        let mut values = [3u32; 1024];
        let visit = |start, chunk: &mut [u32]| {
            for item in chunk {
                *item = item.wrapping_add(start as u32);
            }
        };
        let mut frame = || {
            let (left, right) = values.split_at_mut(512);
            let size = NonZeroUsize::new(16).unwrap();
            self.pool.scope(|jobs| {
                jobs.for_each_chunk_mut(left, size, visit);
                jobs.for_each_chunk_mut(right, size, visit);
            });
        };
        for _ in 0..128 {
            frame();
        }
        let before = HeapSnapshot::now();
        for _ in 0..256 {
            frame();
        }
        let delta = HeapSnapshot::now().since(before);
        std::hint::black_box(values);
        delta
            .events
            .map(|event| u32::try_from(event.calls).unwrap())
            .to_vec()
    }

    /// Two independent clients share the same executor. Keep a thread ID per
    /// chunk in this sample so the browser can verify actual participation.
    pub fn calculate(&mut self, rounds: u32) -> Vec<u32> {
        fn batch<'s>(
            jobs: &JobScope<'s, '_>,
            chunks: &'s mut [(u32, Option<ThreadId>)],
            seed: u32,
            rounds: u32,
        ) {
            jobs.for_each_chunk_mut(chunks, NonZeroUsize::MIN, move |i, chunk| {
                let mut value = i as u32 + seed;
                for _ in 0..rounds {
                    value = value.wrapping_mul(0x9e3779b1).rotate_left(11) ^ seed;
                }
                chunk[0] = (value, Some(thread::current().id()));
            });
        }
        let mut left = [(0, None); 128];
        let mut right = left;
        self.pool.scope(|s| {
            batch(s, &mut left, 7, rounds);
            batch(s, &mut right, 11, rounds);
        });
        let mut workers = HashSet::new();
        let mut checksum = 0u32;
        for (value, thread) in left.into_iter().chain(right) {
            checksum = checksum.wrapping_add(value);
            workers.insert(thread.unwrap());
        }
        vec![
            checksum,
            workers.len() as u32,
            u32::from(workers.contains(&thread::current().id())),
        ]
    }
}

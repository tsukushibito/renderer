use sashiko_heap::HeapSnapshot;
use sashiko_jobs::JobSystem;
use std::hint::black_box;

/// Measure entry, two borrowed submissions, and one range independently. Each
/// row contains host p50/p95 (microseconds) and alloc/zeroed/realloc/free.
pub fn measure(team: &mut JobSystem, now_us: impl Fn() -> f64) -> Vec<f64> {
    let mut result = Vec::with_capacity(18);
    for workload in 0..3 {
        let mut values = [0u32; 1024];
        let mut frame = || {
            team.scope(|s| match workload {
                0 => {
                    black_box(s.parallelism());
                }
                1 => {
                    let (a, b) = values.split_at_mut(512);
                    s.spawn(|| black_box(a)[0] += 1);
                    s.spawn(|| black_box(b)[0] += 1);
                }
                _ => s.for_each_chunk_mut(&mut values, 32.try_into().unwrap(), |_, chunk| {
                    for value in chunk {
                        *value += 1;
                    }
                }),
            });
        };
        for _ in 0..128 {
            frame();
        }
        let mut elapsed = [0.; 256];
        let before = HeapSnapshot::now();
        for sample in &mut elapsed {
            let start = now_us();
            frame();
            *sample = now_us() - start;
        }
        let delta = HeapSnapshot::now().since(before);
        assert!(!delta.overflowed);
        black_box(values);
        elapsed.sort_unstable_by(f64::total_cmp);
        result.extend([elapsed[127], elapsed[243]]);
        result.extend(delta.events.map(|event| event.calls as f64));
    }
    result
}

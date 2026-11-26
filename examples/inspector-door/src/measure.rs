use super::Game;
use sashiko_heap::{CountingAllocator, HeapSnapshot};
use std::{
    alloc::System,
    error::Error,
    hint::black_box,
    time::{Duration, Instant},
};

#[global_allocator]
static HEAP: CountingAllocator<System> = CountingAllocator::new(System);

fn sample(n: usize, journal_bytes: usize, mut update: impl FnMut()) {
    for _ in 0..512 {
        update()
    }
    let mut times = vec![Duration::ZERO; n];
    let start = HeapSnapshot::now();
    for time in &mut times {
        let before = Instant::now();
        update();
        *time = before.elapsed();
    }
    let counts = HeapSnapshot::now().since(start).events;
    times.sort_unstable();
    println!(
        "{{\"ticks\":{n},\"p95_ns\":{},\"max_ns\":{},\"alloc\":{},\"realloc\":{},\"free\":{},\"journal_bytes\":{journal_bytes}}}",
        times[(n * 95).div_ceil(100) - 1].as_nanos(),
        times[n - 1].as_nanos(),
        counts[0].calls + counts[1].calls,
        counts[2].calls,
        counts[3].calls
    );
}

pub fn run(mode: &str, n: usize) -> Result<(), Box<dyn Error>> {
    if !(1..=1_000_000).contains(&n) {
        return Err("expected 1..1000000 samples".into());
    }
    match mode {
        "--measure" => {
            let mut game = Game::new();
            sample(n, 0, || {
                game.advance();
                black_box(&game);
            });
        }
        #[cfg(feature = "live-inspector")]
        "--measure-observed" => {
            let mut session = sashiko::inspect::Session::new(Game::new());
            session.watch("bench", "unlock_denied")?;
            sample(n, session.journal_bytes(), || {
                session.step();
                drop(black_box(session.poll()));
                black_box(session.world());
            });
        }
        _ => return Err("unknown measurement".into()),
    }
    Ok(())
}

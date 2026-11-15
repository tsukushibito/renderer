//! Standalone measurement, without libtest activity in the measurement process.

use renderer::measurement::CountingAllocator;
use std::alloc::System;

#[path = "support/frame_memory.rs"]
mod frame_memory;

#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "cpu".into());
    let baseline = match args.next().as_deref() {
        None => false,
        Some("--baseline") => true,
        Some(_) => panic!("expected optional --baseline"),
    };
    assert!(args.next().is_none(), "unexpected argument");
    match mode.as_str() {
        "cpu" => frame_memory::cpu_lifecycle(baseline),
        _ => panic!("supported mode: cpu [--baseline]"),
    }
}

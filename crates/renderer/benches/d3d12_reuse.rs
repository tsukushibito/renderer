//! CPU-only measurement of the production D3D12 candidate selector.
//! Native resources are replaced only at the ownership boundary with Arc<()>;
//! selection, class indexing, generation checks and lease acquisition/release
//! are timed. Preparation and report formatting are outside each sample.

use std::{alloc::System, hint::black_box, sync::Arc, time::Instant};

#[path = "../src/measurement.rs"]
#[allow(
    dead_code,
    unused_imports,
    reason = "standalone benchmark uses only Rust allocation counters"
)]
mod measurement;
#[path = "../src/backends/d3d12/registry/reuse.rs"]
#[allow(
    dead_code,
    unused_imports,
    reason = "private CPU selector is shared with the benchmark"
)]
mod reuse;

use measurement::{CountingAllocator, Snapshot};
use reuse::{ReuseClass, ReuseIndex};

#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);

struct Entry {
    physical: Arc<()>,
    class: ReuseClass,
    registered: bool,
    generation: u32,
}

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    registered: usize,
    busy: usize,
    transient: usize,
    classes: usize,
}

fn class_key(ordinal: usize) -> ReuseClass {
    if ordinal == 0 {
        ReuseClass::Buffer
    } else {
        ReuseClass::Image {
            width: 64 + ordinal as u32,
            height: 64,
            format: 0,
            mip_levels: 1,
            array_layers: 1,
        }
    }
}

fn main() {
    let reference = std::env::args().any(|argument| argument == "--reference");
    let samples = std::env::var("RENDERER_REUSE_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>().expect("sample count"))
        .unwrap_or(31);
    let iterations = std::env::var("RENDERER_REUSE_ITERATIONS")
        .ok()
        .map(|value| value.parse::<usize>().expect("iteration count"))
        .unwrap_or(256);
    assert!(samples > 0 && iterations > 0);
    let cases = [
        Case {
            name: "small",
            registered: 0,
            busy: 0,
            transient: 8,
            classes: 1,
        },
        Case {
            name: "registered_prefix",
            registered: 1024,
            busy: 0,
            transient: 8,
            classes: 1,
        },
        Case {
            name: "many_transients",
            registered: 0,
            busy: 0,
            transient: 512,
            classes: 1,
        },
        Case {
            name: "mixed_classes",
            registered: 1024,
            busy: 0,
            transient: 512,
            classes: 16,
        },
        Case {
            name: "busy_candidates",
            registered: 1024,
            busy: 256,
            transient: 64,
            classes: 4,
        },
    ];
    let reports: Vec<_> = cases
        .into_iter()
        .map(|case| {
            if reference {
                run::<true>(case, samples, iterations)
            } else {
                run::<false>(case, samples, iterations)
            }
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "scope": "production CPU selector, eligibility and Arc lease acquire/return; no GPU/native calls",
            "selector": if reference { "pre-index production scan" } else { "prepared class index" },
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "minimum_warmup_frames": 1024,
        "minimum_warmup_ms": 100,
        "samples": samples,
        "frames_per_sample": iterations,
        "cases": reports,
    })).unwrap());
}

fn run<const REFERENCE: bool>(case: Case, samples: usize, iterations: usize) -> serde_json::Value {
    let count = case.registered + case.busy + case.transient;
    let mut entries: Vec<_> = (0..count)
        .map(|index| Entry {
            physical: Arc::new(()),
            class: class_key(index.saturating_sub(case.registered) % case.classes),
            registered: index < case.registered,
            generation: 0,
        })
        .collect();
    let _busy: Vec<_> = entries[case.registered..case.registered + case.busy]
        .iter()
        .map(|entry| Arc::clone(&entry.physical))
        .collect();
    let mut index = if REFERENCE {
        None
    } else {
        Some(ReuseIndex::new(count).unwrap())
    };
    if let Some(index) = &mut index {
        for (position, entry) in entries.iter().enumerate() {
            index.install(position, entry.class);
            if entry.registered {
                index.remove(position);
            }
        }
    }
    let mut leases = vec![None; case.transient];
    let queries: Vec<_> = (0..case.transient)
        .map(|position| class_key(position % case.classes))
        .collect();
    let warmup_started = Instant::now();
    let mut warmup_frames = 0;
    while warmup_frames < 1024 || warmup_started.elapsed().as_millis() < 100 {
        black_box(frame::<REFERENCE>(
            &mut index,
            &mut entries,
            &queries,
            &mut leases,
        ));
        warmup_frames += 1;
    }
    let mut elapsed = Vec::with_capacity(samples);
    let mut allocation = Snapshot::default();
    for _ in 0..samples {
        let before = Snapshot::now();
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(frame::<REFERENCE>(
                &mut index,
                &mut entries,
                &queries,
                &mut leases,
            ));
        }
        let duration = started.elapsed();
        allocation.accumulate(Snapshot::now().since(before));
        elapsed.push(duration.as_nanos() as f64 / iterations as f64);
    }
    assert!(
        allocation.is_allocation_free(),
        "normal selection must not allocate: {allocation:?}"
    );
    let mut sorted = elapsed.clone();
    sorted.sort_by(f64::total_cmp);
    serde_json::json!({
        "name": case.name,
        "registered": case.registered,
        "busy": case.busy,
        "transient": case.transient,
        "classes": case.classes,
        "warmup_frames": warmup_frames,
        "median_frame_ns": sorted[sorted.len() / 2],
        "p95_frame_ns": sorted[(sorted.len() * 95).div_ceil(100).saturating_sub(1)],
        "raw_frame_ns": elapsed,
        "rust_allocation_calls": allocation.rust.map(|event| event.calls),
        "rust_allocation_bytes": allocation.rust.map(|event| event.bytes),
    })
}

fn frame<const REFERENCE: bool>(
    index: &mut Option<ReuseIndex<ReuseClass>>,
    entries: &mut [Entry],
    queries: &[ReuseClass],
    leases: &mut [Option<Arc<()>>],
) -> usize {
    if let Some(index) = index {
        index.begin();
    }
    let mut checksum = 0;
    for (&class, lease) in queries.iter().zip(leases.iter_mut()) {
        let position = if REFERENCE {
            // Preserve the former production iterator and predicate order,
            // including its direct entry reference rather than indexed reads.
            entries.iter().position(|entry| {
                !entry.registered
                    && Arc::strong_count(&entry.physical) == 1
                    && entry.generation < u32::MAX
                    && entry.class == class
            })
        } else {
            index.as_mut().unwrap().select(class, |position| {
                let entry = &entries[position];
                !entry.registered
                    && Arc::strong_count(&entry.physical) == 1
                    && entry.generation < u32::MAX
                    && entry.class == class
            })
        }
        .expect("prepared unregistered resources");
        let entry = &mut entries[position];
        entry.generation += 1;
        *lease = Some(Arc::clone(&entry.physical));
        checksum += position;
    }
    leases.fill(None);
    checksum
}

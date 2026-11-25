//! Disjoint process-counter intervals across consecutive presentation notifications.
//! Each draw retry contributes to the same frame, including time in the event host.
use super::metrics::Measurements;
use sashiko_renderer::core::measurement::Snapshot;

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Game,
    Scene,
    Graph,
    Backend,
    Metrics,
    Host,
}

impl Phase {
    pub(super) const NAMES: [&'static str; 6] =
        ["game", "scene", "graph", "backend", "metrics", "host"];
}

pub(super) struct FrameCounts {
    pub(super) total: Snapshot,
    pub(super) phases: [[u64; 4]; Phase::NAMES.len()],
}

pub(super) struct FrameAllocations {
    start: Snapshot,
    last: Snapshot,
    phases: [[u64; 4]; Phase::NAMES.len()],
}

pub(super) fn mark(samples: &mut Option<Measurements>, phase: Phase) {
    if let Some(samples) = samples {
        samples.allocations.finish(phase, Snapshot::now());
    }
}

impl FrameAllocations {
    pub(super) fn new() -> Self {
        let start = Snapshot::now();
        Self {
            start,
            last: start,
            phases: Default::default(),
        }
    }

    fn finish(&mut self, phase: Phase, now: Snapshot) {
        let counts = &mut self.phases[phase as usize];
        for (count, delta) in counts.iter_mut().zip(now.since(self.last).rust) {
            *count = count.saturating_add(delta.calls);
        }
        self.last = now;
    }

    pub(super) fn presented(&mut self) -> FrameCounts {
        let now = Snapshot::now();
        self.finish(Phase::Host, now);
        let total = now.since(self.start);
        self.start = now;
        FrameCounts {
            total,
            phases: std::mem::take(&mut self.phases),
        }
    }
}

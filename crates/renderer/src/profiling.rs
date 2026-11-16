//! Optional timing values shared by native backends. GPU intervals describe
//! recorded commands; they never establish presentation or display completion.

use std::time::Duration;

/// GPU elapsed time for the whole recorded Graph, not display latency or FPS.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum GpuTiming {
    Available {
        elapsed: Duration,
        timestamp_period_ns: f64,
        valid_bits: u32,
    },
    /// A completed query was unexpectedly unavailable; no duration is inferred.
    NotReady,
    /// The sample's host lifetime cannot exclude multiple counter wraps.
    AmbiguousCounterWrap,
}

/// Optional CPU phases and the GPU Graph interval. Build/compile are caller work.
#[derive(Clone, Copy, Debug)]
pub struct SubmissionMetrics {
    /// Resource and command planning within the explicitly prepared working set.
    pub cpu_prepare: Duration,
    /// Command recording, including its begin/end calls.
    pub cpu_record: Duration,
    /// Native submit and the Runtime's acceptance bookkeeping.
    pub cpu_submit: Duration,
    /// Completion bookkeeping, resource retirement and query result copying.
    /// Excludes time blocked in a Fence wait and unsuccessful polling attempts.
    pub cpu_retire: Duration,
    pub gpu: GpuTiming,
}

/// Optional presentation CPU phases and fullscreen GPU command interval.
/// Acquire and present measure their CPU API calls, including any native pacing
/// cost. GPU time excludes presentation-engine/VSync and display completion.
/// Each sample describes a fullscreen GPU submission, including one whose
/// display request was rejected. Failed acquire attempts have no GPU sample;
/// callers should separately measure all API attempts and their outcomes.
#[derive(Clone, Copy, Debug)]
pub struct PresentationMetrics {
    pub cpu_acquire: Duration,
    /// Source planning and slot reservation, excluding native acquire.
    pub cpu_prepare: Duration,
    pub cpu_record: Duration,
    pub cpu_submit: Duration,
    pub cpu_present: Duration,
    /// Completion bookkeeping/query copying, excluding explicit waits and
    /// unsuccessful polls.
    pub cpu_retire: Duration,
    pub gpu: GpuTiming,
}

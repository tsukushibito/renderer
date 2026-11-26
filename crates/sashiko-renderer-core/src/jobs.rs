//! CPU jobs used by Graph construction and command recording.
pub use sashiko_jobs::{JobSystem, SharedJobs};

/// Native recording is selected independently of CPU scene and Graph work.
/// Each backend supplies the smallest balanced workload verified for its recorder.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(crate) struct RecordingTeam {
    pub jobs: SharedJobs,
    pub automatic: bool,
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
impl RecordingTeam {
    pub const DRAWS_PER_BATCH: usize = 3_125;
    pub const POOL_CAPACITY: usize = 64;

    /// A balanced split at the target batch size can produce pieces as small
    /// as half that size. Ignore tiny batches when estimating useful work.
    pub fn large_enough(&self, min_batches: usize, batches: impl Iterator<Item = usize>) -> bool {
        if !self.automatic {
            return true;
        }
        let (count, draws) = batches
            .filter(|&n| n >= Self::DRAWS_PER_BATCH.div_ceil(2))
            .fold((0, 0usize), |(count, sum), n| {
                (count + 1, sum.saturating_add(n))
            });
        self.jobs.parallelism().get() >= 4
            && count >= min_batches
            && draws >= Self::DRAWS_PER_BATCH * min_batches
    }
}

#[cfg(all(
    test,
    not(target_arch = "wasm32"),
    any(feature = "vulkan", all(windows, feature = "d3d12"))
))]
mod tests {
    use super::*;

    use std::num::NonZeroUsize;

    #[test]
    fn automatic_recording_requires_enough_independent_work() {
        let team = RecordingTeam {
            jobs: SharedJobs::new(JobSystem::new(NonZeroUsize::new(4).unwrap()).unwrap()),
            automatic: true,
        };
        assert!(team.large_enough(16, [3_125; 16].into_iter().chain([1])));
        assert!(team.large_enough(8, [3_125; 8].into_iter()));
        assert!(!team.large_enough(16, [3_125; 8].into_iter()));
        assert!(!team.large_enough(8, [12_500; 4].into_iter()));
        assert!(!team.large_enough(16, [64; 16].into_iter()));
        assert!(!team.large_enough(8, [50_000].into_iter().chain([1; 15])));
    }
}

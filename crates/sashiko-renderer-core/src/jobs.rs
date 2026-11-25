//! One host-supplied worker team shared across renderer stages.
pub use sashiko_jobs::JobSystem;
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex, TryLockError},
};

/// Cloning this handle shares the existing workers. Each scope completes before
/// its caller continues. Overlapping host stages can make progress inline.
#[derive(Clone)]
pub struct RendererJobs {
    team: Arc<Mutex<JobSystem>>,
    participants: NonZeroUsize,
}

impl RendererJobs {
    pub fn new(team: JobSystem) -> Self {
        Self {
            participants: team.parallelism(),
            team: Arc::new(Mutex::new(team)),
        }
    }

    /// Maximum participants, including the host that enters the scope.
    pub fn parallelism(&self) -> NonZeroUsize {
        self.participants
    }

    /// Borrow the shared team for one stage. A nested or concurrent stage
    /// runs on its caller rather than waiting on a worker that it may own.
    pub fn with<R>(&self, run: impl FnOnce(&mut JobSystem) -> R) -> R {
        match self.team.try_lock() {
            Ok(mut team) => run(&mut team),
            Err(TryLockError::Poisoned(error)) => run(&mut error.into_inner()),
            Err(TryLockError::WouldBlock) => run(&mut JobSystem::inline()),
        }
    }
}

impl From<JobSystem> for RendererJobs {
    fn from(jobs: JobSystem) -> Self {
        Self::new(jobs)
    }
}

/// Native recording is selected independently of CPU scene and Graph work.
/// Each backend supplies the smallest balanced workload verified for its recorder.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(crate) struct RecordingTeam {
    pub jobs: RendererJobs,
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn nested_stage_finishes_inline_and_returns_team_to_next_stage() {
        let two = NonZeroUsize::new(2).unwrap();
        let owner = RendererJobs::new(JobSystem::new(two).unwrap());
        let other = owner.clone();
        owner.with(|team| {
            assert_eq!(team.parallelism(), two);
            team.scope(|scope| {
                scope.spawn(|| other.with(|nested| assert_eq!(nested.parallelism().get(), 1)));
            });
        });
        other.with(|team| assert_eq!(team.parallelism(), two));
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    #[test]
    fn automatic_recording_requires_enough_independent_work() {
        let team = RecordingTeam {
            jobs: RendererJobs::new(JobSystem::new(NonZeroUsize::new(4).unwrap()).unwrap()),
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

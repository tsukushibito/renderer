//! Share one host worker team across subsystems.
use crate::JobSystem;
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex, TryLockError},
};

/// Cloning this handle shares the existing workers. Each scope completes before
/// its caller continues. Overlapping host stages can make progress inline.
#[derive(Clone)]
pub struct SharedJobs {
    team: Arc<Mutex<JobSystem>>,
    participants: NonZeroUsize,
}

impl SharedJobs {
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

impl From<JobSystem> for SharedJobs {
    fn from(jobs: JobSystem) -> Self {
        Self::new(jobs)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn nested_stage_finishes_inline_and_returns_team_to_next_stage() {
        let two = NonZeroUsize::new(2).unwrap();
        let owner = SharedJobs::new(JobSystem::new(two).unwrap());
        let other = owner.clone();
        owner.with(|team| {
            assert_eq!(team.parallelism(), two);
            team.scope(|scope| {
                scope.spawn(|| other.with(|nested| assert_eq!(nested.parallelism().get(), 1)));
            });
        });
        other.with(|team| assert_eq!(team.parallelism(), two));
    }
}

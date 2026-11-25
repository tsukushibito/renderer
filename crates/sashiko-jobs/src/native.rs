use super::scheduler::Scheduler;
use std::sync::Arc;
use std::thread::{Builder, JoinHandle};

pub(super) struct Workers {
    pub(super) scheduler: Arc<Scheduler>,
    threads: Vec<JoinHandle<()>>,
}

impl Workers {
    pub(super) fn new(helpers: usize) -> std::io::Result<Self> {
        let scheduler = Arc::new(Scheduler::new(helpers)?);
        let mut team = Self {
            scheduler,
            threads: Vec::new(),
        };
        team.threads
            .try_reserve_exact(helpers)
            .map_err(std::io::Error::other)?;
        for i in 0..helpers {
            let scheduler = team.scheduler.clone();
            team.threads.push(
                Builder::new()
                    .name(format!("sashiko-job-{i}"))
                    .spawn(move || scheduler.worker())?,
            );
        }
        team.scheduler.wait_ready();
        Ok(team)
    }
}
impl Drop for Workers {
    fn drop(&mut self) {
        self.scheduler.stop();
        for thread in self.threads.drain(..) {
            thread.join().expect("job worker failed");
        }
    }
}

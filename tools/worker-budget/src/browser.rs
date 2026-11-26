use super::{Executor, measure};
use sashiko_jobs::JobSystem;
use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};
use wasm_bindgen::prelude::*;

// The JS coordinator starts all module workers before letting any enter Rayon.
// Each shared-memory ticket transfers ownership of one Rayon worker exactly once.
static PENDING: Mutex<BTreeMap<u32, rayon::ThreadBuilder>> = Mutex::new(BTreeMap::new());
static NEXT: Mutex<u32> = Mutex::new(0);

#[wasm_bindgen]
pub struct Trial {
    executor: Executor,
    tickets: std::ops::Range<u32>,
}

#[wasm_bindgen]
impl Trial {
    pub fn shared(n: u32) -> Result<Trial, JsValue> {
        if !(1..=8).contains(&n) {
            return Err("expected 1..8 participants".into());
        }
        let mut next = NEXT.lock().unwrap();
        let end = next.checked_add(n).ok_or("ticket space exhausted")?;
        let tickets = *next..end;
        *next = end;
        let mut pending = PENDING.lock().unwrap();
        let result = rayon::ThreadPoolBuilder::new()
            .num_threads(n as usize)
            .spawn_handler(|thread| {
                pending.insert(tickets.start + thread.index() as u32, thread);
                Ok(())
            })
            .build();
        match result {
            Ok(pool) => Ok(Self {
                executor: Executor::Shared(super::Jobs(Arc::new(pool))),
                tickets,
            }),
            Err(error) => {
                pending.retain(|id, _| !tickets.contains(id));
                Err(error.to_string().into())
            }
        }
    }

    pub async fn current(n: usize) -> Result<Trial, JsValue> {
        let team = JobSystem::start_web(NonZeroUsize::new(n).ok_or("zero participants")?).await?;
        Ok(Self {
            executor: Executor::Current(team, None),
            tickets: 0..0,
        })
    }

    pub fn first(&self) -> u32 {
        self.tickets.start
    }

    pub fn verify(&mut self) -> Vec<f64> {
        super::verify(&mut self.executor).map(|x| x as f64).to_vec()
    }

    pub fn survivor(&self, late: bool) -> Vec<f64> {
        super::survivor_frame(&self.executor, late)
    }

    pub fn sample(&mut self, heavy: bool, physics: bool, jobs: bool, idle: bool) -> Vec<f64> {
        #[wasm_bindgen]
        extern "C" {
            fn pauseTrial();
        }
        measure(&mut self.executor, heavy, physics, jobs, || {
            if idle {
                pauseTrial();
            }
        })
    }
}

impl Drop for Trial {
    fn drop(&mut self) {
        PENDING
            .lock()
            .unwrap()
            .retain(|id, _| !self.tickets.contains(id));
    }
}

#[wasm_bindgen]
pub fn serve(ticket: u32) -> Result<(), JsValue> {
    let worker = PENDING
        .lock()
        .unwrap()
        .remove(&ticket)
        .ok_or("expired trial ticket")?;
    worker.run();
    Ok(())
}

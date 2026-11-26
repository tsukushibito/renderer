#[cfg(not(target_feature = "atomics"))]
compile_error!("web-threads requires a shared-memory Wasm build with atomics enabled");

use super::{Backend, JobSystem, scheduler::Scheduler};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, Weak};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::js_sys::{JsString, Promise};

// Single-use tickets across instances of the same shared Wasm memory. JS never
// receives a Rust pointer. Retiring a system invalidates unclaimed tickets.
struct Registry {
    next: u32,
    tickets: BTreeMap<u32, Ticket>,
}
static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    next: 0,
    tickets: BTreeMap::new(),
});

#[wasm_bindgen(module = "/src/workers.js")]
extern "C" {
    type Team;
    #[wasm_bindgen(catch,js_name=createTeam)]
    fn create_team(
        module: JsValue,
        memory: JsValue,
        glue: &JsString,
        first: u32,
        count: u32,
    ) -> Result<Team, JsValue>;
    #[wasm_bindgen(method, getter)]
    fn ready(this: &Team) -> Promise;
    #[wasm_bindgen(method)]
    fn stop(this: &Team);
}

enum Ticket {
    Custom(Weak<Scheduler>),
    #[cfg(feature = "experimental-rayon")]
    Rayon(rayon::ThreadBuilder),
}

pub(super) struct Workers {
    pub(super) scheduler: Arc<Scheduler>,
    lease: Lease,
}
impl Drop for Workers {
    fn drop(&mut self) {
        self.scheduler.stop();
    }
}

#[cfg(feature = "experimental-rayon")]
pub(super) struct RayonWorkers {
    // Release the registry before requesting JS worker shutdown.
    pub(super) pool: rayon::ThreadPool,
    lease: Lease,
}

struct Lease {
    tickets: std::ops::Range<u32>,
    team: Option<Team>,
}

impl Lease {
    fn reserve(count: usize) -> Result<Self, JsValue> {
        if !web_sys::js_sys::global().is_instance_of::<web_sys::DedicatedWorkerGlobalScope>() {
            return Err("parallel jobs require a dedicated host Worker".into());
        }
        let mut registry = REGISTRY.lock().unwrap();
        let first = registry.next;
        registry.next = first
            .checked_add(count as u32)
            .ok_or("job worker ticket space exhausted")?;
        Ok(Self {
            tickets: first..registry.next,
            team: None,
        })
    }
    async fn launch(&mut self) -> Result<(), JsValue> {
        #[wasm_bindgen]
        extern "C" {
            #[wasm_bindgen(thread_local_v2,js_namespace=["import","meta"],js_name=url)]
            static GLUE: JsString;
        }
        let team = GLUE.with(|glue| {
            create_team(
                wasm_bindgen::module(),
                wasm_bindgen::memory(),
                glue,
                self.tickets.start,
                self.tickets.end - self.tickets.start,
            )
        })?;
        let ready = team.ready();
        self.team = Some(team);
        JsFuture::from(ready).await?;
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut registry = REGISTRY.lock().unwrap();
        for id in self.tickets.clone() {
            registry.tickets.remove(&id);
        }
        drop(registry);
        if let Some(team) = &self.team {
            team.stop();
        }
    }
}

impl JobSystem {
    /// Start N-1 module workers in the dedicated host Worker.
    pub async fn start_web(parallelism: NonZeroUsize) -> Result<Self, JsValue> {
        let helpers = parallelism.get() - 1;
        if helpers == 0 {
            return Ok(Self::inline());
        }
        let lease = Lease::reserve(helpers)?;
        let scheduler = Arc::new(Scheduler::new(helpers).map_err(|e| e.to_string())?);
        {
            let mut registry = REGISTRY.lock().unwrap();
            for id in lease.tickets.clone() {
                registry
                    .tickets
                    .insert(id, Ticket::Custom(Arc::downgrade(&scheduler)));
            }
        }
        let mut workers = Workers { scheduler, lease };
        workers.lease.launch().await?;
        Ok(Self {
            backend: Backend::Workers(workers),
        })
    }

    /// Start an isolated experimental Rayon pool in N-1 module workers.
    #[cfg(feature = "experimental-rayon")]
    pub async fn start_web_rayon(parallelism: NonZeroUsize) -> Result<Self, JsValue> {
        let helpers = parallelism.get() - 1;
        if helpers == 0 {
            return Ok(Self::inline());
        }
        if helpers > rayon::max_num_threads() {
            return Err("too many Rayon workers".into());
        }
        let lease = Lease::reserve(helpers)?;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(helpers)
            .spawn_handler(|thread| {
                let id = lease.tickets.start + thread.index() as u32;
                REGISTRY
                    .lock()
                    .unwrap()
                    .tickets
                    .insert(id, Ticket::Rayon(thread));
                Ok(())
            })
            .build()
            .map_err(|e| e.to_string())?;
        let mut workers = RayonWorkers { pool, lease };
        workers.lease.launch().await?;
        Ok(Self {
            backend: Backend::Rayon(workers),
        })
    }
}

enum Worker {
    Custom(Arc<Scheduler>),
    #[cfg(feature = "experimental-rayon")]
    Rayon(rayon::ThreadBuilder),
}
/// Worker-side bootstrap capability, consumed by the library's JS module.
#[wasm_bindgen]
#[doc(hidden)]
pub struct SashikoWorker(Worker);

#[wasm_bindgen]
impl SashikoWorker {
    #[wasm_bindgen(js_name=claim)]
    pub fn claim(ticket: u32) -> Result<SashikoWorker, JsValue> {
        let ticket = REGISTRY
            .lock()
            .unwrap()
            .tickets
            .remove(&ticket)
            .ok_or("expired job worker ticket")?;
        Ok(Self(match ticket {
            Ticket::Custom(weak) => Worker::Custom(weak.upgrade().ok_or("retired worker team")?),
            #[cfg(feature = "experimental-rayon")]
            Ticket::Rayon(thread) => Worker::Rayon(thread),
        }))
    }
    pub fn serve(self) {
        match self.0 {
            Worker::Custom(scheduler) => scheduler.worker(),
            #[cfg(feature = "experimental-rayon")]
            Worker::Rayon(thread) => thread.run(),
        }
    }
}

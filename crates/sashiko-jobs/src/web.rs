#[cfg(not(target_feature = "atomics"))]
compile_error!("web-threads requires a shared-memory Wasm build with atomics enabled");

use super::{JobSystem, scheduler::Scheduler};
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
    tickets: BTreeMap<u32, Weak<Scheduler>>,
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

pub(super) struct Workers {
    pub(super) scheduler: Arc<Scheduler>,
    tickets: std::ops::Range<u32>,
    team: Option<Team>,
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.scheduler.stop();
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
    /// Start N−1 module Workers in this dedicated host Worker. N includes the
    /// host. Initialization failures retire the partially started team.
    pub async fn start_web(parallelism: NonZeroUsize) -> Result<Self, JsValue> {
        if parallelism == NonZeroUsize::MIN {
            return Ok(Self::inline());
        }
        if !web_sys::js_sys::global().is_instance_of::<web_sys::DedicatedWorkerGlobalScope>() {
            return Err("parallel jobs require a dedicated host Worker".into());
        }
        let helpers = parallelism.get() - 1;
        let scheduler =
            Arc::new(Scheduler::new(helpers).map_err(|e| JsValue::from_str(&e.to_string()))?);
        let tickets = {
            let mut registry = REGISTRY.lock().unwrap();
            let first = registry.next;
            registry.next = first
                .checked_add(helpers as u32)
                .ok_or("job worker ticket space exhausted")?;
            let range = first..registry.next;
            for id in range.clone() {
                registry.tickets.insert(id, Arc::downgrade(&scheduler));
            }
            range
        };
        let mut workers = Workers {
            scheduler,
            tickets,
            team: None,
        };
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
                workers.tickets.start,
                helpers as u32,
            )
        })?;
        let ready = team.ready();
        workers.team = Some(team);
        JsFuture::from(ready).await?;
        Ok(Self {
            workers: Some(workers),
        })
    }
}

/// Worker-side bootstrap capability, consumed by the library's JS module.
#[wasm_bindgen]
#[doc(hidden)]
pub struct SashikoWorker(Arc<Scheduler>);

#[wasm_bindgen]
impl SashikoWorker {
    #[wasm_bindgen(js_name=claim)]
    pub fn claim(ticket: u32) -> Result<SashikoWorker, JsValue> {
        let scheduler = REGISTRY
            .lock()
            .unwrap()
            .tickets
            .remove(&ticket)
            .and_then(|w| w.upgrade())
            .ok_or("expired job worker ticket")?;
        Ok(Self(scheduler))
    }
    pub fn serve(self) {
        self.0.worker();
    }
}

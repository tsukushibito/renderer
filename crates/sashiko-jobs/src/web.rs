use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use wasm_bindgen_futures::JsFuture;
use web_sys::wasm_bindgen::{JsCast, JsValue};

use super::{Backend, JobPool};

impl JobPool {
    /// Initialize once in a dedicated host Worker before running parallel work.
    /// Share the returned handle throughout that host. Child workers live for
    /// the lifetime of the host Worker and its shared Wasm memory.
    pub async fn start_web(workers: NonZeroUsize) -> Result<Self, JsValue> {
        static STARTED: AtomicBool = AtomicBool::new(false);
        let global = web_sys::js_sys::global();
        if !global.is_instance_of::<web_sys::DedicatedWorkerGlobalScope>() {
            return Err("parallel jobs require a dedicated host Worker".into());
        }
        if workers.get() > rayon::max_num_threads() {
            return Err("worker count exceeds pool limit".into());
        }
        if STARTED.swap(true, Ordering::AcqRel) {
            return Err("Web pool initialization already attempted in this host".into());
        }
        JsFuture::from(wasm_bindgen_rayon::init_thread_pool(workers.get())).await?;
        let entry = super::entry::Entry::new();
        rayon::spawn(entry.service());
        entry.wait_ready();
        Ok(Self(Backend::Web(entry)))
    }
}

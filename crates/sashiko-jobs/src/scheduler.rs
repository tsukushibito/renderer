//! A task owns its capture in aligned inline storage. Erasure is private to
//! scoped submission: drain waits for every callback (including descendants).
use std::cell::Cell;
use std::collections::VecDeque;
use std::mem::{MaybeUninit, align_of, size_of};

use super::scope::Group;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Condvar, Mutex};

thread_local! { static LANE: Cell<usize> = const {Cell::new(0)}; }
const CAPACITY: usize = 16;
#[repr(C, align(64))]
struct Storage([MaybeUninit<u8>; 128]);
struct Task {
    storage: Storage,
    invoke: unsafe fn(&mut Storage),
    group: *const Group,
}
// SAFETY: only Send closures are erased. Group lives through the final completion.
unsafe impl Send for Task {}

// Keep independent owners' hot locks and availability hints on separate cache lines.
#[repr(align(64))]
struct LocalQueue {
    tasks: Mutex<VecDeque<Task>>,
    available: AtomicBool,
}
impl LocalQueue {
    fn pop(&self, owner: bool) -> Option<Task> {
        if !self.available.load(SeqCst) {
            return None;
        }
        let mut tasks = self.tasks.lock().unwrap();
        let task = if owner {
            tasks.pop_back()
        } else {
            tasks.pop_front()
        };
        if tasks.is_empty() {
            self.available.store(false, SeqCst);
        }
        task
    }
}
pub(crate) struct Scheduler {
    #[cfg(feature = "scheduler-metrics")]
    pub(crate) counters: [AtomicUsize; 4],
    #[cfg(all(feature = "scheduler-metrics", not(target_arch = "wasm32")))]
    pub(crate) join_wait_ns: std::sync::atomic::AtomicU64,
    queues: Vec<LocalQueue>,
    gate: Mutex<()>,
    wake: Condvar,
    sleeping: AtomicUsize,
    joining: AtomicUsize,
    ready: AtomicUsize,
    stopped: AtomicBool,
    pub(crate) helpers: usize,
}
impl Scheduler {
    pub(crate) fn new(helpers: usize) -> std::io::Result<Self> {
        let n = helpers
            .checked_add(1)
            .ok_or(std::io::ErrorKind::InvalidInput)?;
        let mut queues = Vec::new();
        queues.try_reserve_exact(n).map_err(std::io::Error::other)?;
        for _ in 0..n {
            let mut q = VecDeque::new();
            q.try_reserve_exact(CAPACITY)
                .map_err(std::io::Error::other)?;
            queues.push(LocalQueue {
                tasks: Mutex::new(q),
                available: false.into(),
            });
        }
        Ok(Self {
            #[cfg(feature = "scheduler-metrics")]
            counters: Default::default(),
            #[cfg(all(feature = "scheduler-metrics", not(target_arch = "wasm32")))]
            join_wait_ns: Default::default(),
            queues,
            gate: Mutex::new(()),
            wake: Condvar::new(),
            sleeping: 0.into(),
            joining: 0.into(),
            ready: 0.into(),
            stopped: false.into(),
            helpers,
        })
    }
    pub(crate) fn lane(&self) -> usize {
        LANE.get() % self.queues.len()
    }
    pub(crate) fn needs_split(&self, owner: usize) -> bool {
        // The owner already has more ranges for peers to take. Avoid adding a
        // second generation of tasks while those initial ranges are available.
        !self.queues[owner]
            .available
            .load(std::sync::atomic::Ordering::Relaxed)
            && self.sleeping.load(std::sync::atomic::Ordering::Relaxed) != 0
    }
    fn signal(&self, all: bool) {
        if self.sleeping.load(SeqCst) != 0 {
            let _gate = self.gate.lock().unwrap();
            if all {
                self.wake.notify_all()
            } else {
                #[cfg(feature = "scheduler-metrics")]
                self.counters[3].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.wake.notify_one()
            }
        }
    }
    /// SAFETY: the caller drains group before any data borrowed by f expires.
    pub(crate) unsafe fn publish<F: FnOnce() + Send>(&self, group: &Group, f: F) -> Result<(), F> {
        if size_of::<F>() > size_of::<Storage>() || align_of::<F>() > align_of::<Storage>() {
            return Err(f);
        }
        let local = &self.queues[self.lane()];
        let mut queue = local.tasks.lock().unwrap();
        if queue.len() == CAPACITY {
            return Err(f);
        }
        unsafe fn invoke<F: FnOnce()>(storage: &mut Storage) {
            // SAFETY: publish stored exactly one aligned F here. Removal transfers ownership.
            let f = unsafe { storage.0.as_mut_ptr().cast::<F>().read() };
            f();
        }
        let mut task = Task {
            storage: Storage([MaybeUninit::uninit(); 128]),
            invoke: invoke::<F>,
            group,
        };
        // SAFETY: the size/alignment checks above fit F at offset zero in this unused storage.
        unsafe {
            task.storage.0.as_mut_ptr().cast::<F>().write(f);
        }
        group.pending.fetch_add(1, SeqCst);
        queue.push_back(task);
        local.available.store(true, SeqCst);
        #[cfg(feature = "scheduler-metrics")]
        self.counters[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        drop(queue);
        self.signal(false);
        Ok(())
    }
    fn take(&self, lane: usize) -> Option<Task> {
        if let Some(task) = self.queues[lane].pop(true) {
            return Some(task);
        }
        for offset in 1..self.queues.len() {
            let victim = (lane + offset) % self.queues.len();
            if let Some(task) = self.queues[victim].pop(false) {
                #[cfg(feature = "scheduler-metrics")]
                self.counters[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Some(task);
            }
            #[cfg(feature = "scheduler-metrics")]
            self.counters[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        None
    }
    fn execute(&self, mut task: Task) {
        // SAFETY: task counted from publication through this call; drain cannot
        // finish while any task can still submit descendants or record a panic.
        let group = unsafe { &*task.group };
        // SAFETY: take removes this task exactly once and invoke matches its stored type.
        group.capture(|| unsafe { (task.invoke)(&mut task.storage) });
        if group.pending.fetch_sub(1, SeqCst) == 1 && self.joining.load(SeqCst) != 0 {
            self.signal(true);
        }
    }
    fn help_until(&self, joining: bool, done: impl Fn() -> bool) {
        let lane = self.lane();
        while !done() {
            if let Some(task) = self.take(lane) {
                self.execute(task);
                continue;
            }
            let gate = self.gate.lock().unwrap();
            self.sleeping.fetch_add(1, SeqCst);
            if joining {
                self.joining.fetch_add(1, SeqCst);
            }
            // Recheck all predicates after registering so a publisher cannot
            // slip between the empty scan and the condition-variable wait.
            let task = if done() { None } else { self.take(lane) };
            if task.is_none() && !done() {
                // A joining thread also executes ready work. Only measure the
                // time it actually enters a wait for outstanding jobs.
                #[cfg(feature = "tracy")]
                let _span = joining.then(|| {
                    tracy_client::Client::running().map(|client| {
                        client.span(tracy_client::span_location!("jobs.join.wait"), 0)
                    })
                });
                #[cfg(all(feature = "scheduler-metrics", not(target_arch = "wasm32")))]
                let waiting = joining.then(std::time::Instant::now);
                drop(self.wake.wait(gate).unwrap());
                #[cfg(all(feature = "scheduler-metrics", not(target_arch = "wasm32")))]
                if let Some(start) = waiting {
                    self.join_wait_ns.fetch_add(
                        start.elapsed().as_nanos() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
            } else {
                drop(gate);
            }
            if joining {
                self.joining.fetch_sub(1, SeqCst);
            }
            self.sleeping.fetch_sub(1, SeqCst);
            if let Some(task) = task {
                self.execute(task);
            }
        }
    }
    pub(crate) fn drain(&self, group: &Group) {
        self.help_until(true, || group.pending.load(SeqCst) == 0);
    }
    pub(crate) fn worker(&self) {
        let lane = self.ready.fetch_add(1, SeqCst) + 1;
        LANE.set(lane);
        self.signal(true);
        self.help_until(false, || self.stopped.load(SeqCst));
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn wait_ready(&self) {
        // Startup only. Workers have no tasks before construction returns.
        self.help_until(false, || self.ready.load(SeqCst) == self.helpers);
    }
    pub(crate) fn stop(&self) {
        self.stopped.store(true, SeqCst);
        self.signal(true);
    }
}

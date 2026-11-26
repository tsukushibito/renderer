//! Compare a host-owned Rayon pool with the current job system on identical CPU work.
use rapier3d::prelude::*;
use rayon::prelude::*;
use sashiko_animation::{Clip, LocalPose, PoseBatch, PoseWorkspace, Skeleton};
use sashiko_heap::{CountingAllocator, HeapSnapshot};
use sashiko_jobs::JobSystem;
use std::{hint::black_box, sync::Arc};

#[global_allocator]
static HEAP: CountingAllocator<std::alloc::System> = CountingAllocator::new(std::alloc::System);

#[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
mod browser;

pub enum Executor {
    Current(JobSystem, Option<Arc<rayon::ThreadPool>>),
    Shared(Jobs),
}

/// Candidate host handle. Clones and nested runs use the same worker budget.
#[derive(Clone)]
pub struct Jobs(Arc<rayon::ThreadPool>);

impl Jobs {
    pub fn run<R: Send>(&self, work: impl FnOnce() -> R + Send) -> R {
        self.0.install(work)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn native(mode: &str, n: std::num::NonZeroUsize) -> Executor {
    let pool = |count| {
        let pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(count)
                .build()
                .unwrap(),
        );
        assert_eq!(pool.broadcast(|_| ()).len(), count);
        pool
    };
    match mode {
        "shared" => Executor::Shared(Jobs(pool(n.get()))),
        "current" => Executor::Current(JobSystem::new(n).unwrap(), None),
        "partitioned" => {
            // The caller waits in step; the remaining helpers process game jobs.
            let physics = n.get().div_ceil(2);
            let team = JobSystem::new((n.get() - physics + 1).try_into().unwrap()).unwrap();
            Executor::Current(team, Some(pool(physics)))
        }
        _ => panic!("unknown executor {mode}"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn now() -> f64 {
    static CLOCK: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    CLOCK
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1e6
}

#[cfg(target_arch = "wasm32")]
fn now() -> f64 {
    #[wasm_bindgen::prelude::wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = performance, js_name = now)]
        fn milliseconds() -> f64;
    }
    milliseconds() * 1000.
}

struct Scene {
    world: PhysicsWorld,
    probe: RigidBodyHandle,
    clip: Clip,
    batches: Vec<(PoseBatch, f32)>,
    visible: Vec<bool>,
}

impl Scene {
    fn new(executor: &Executor, heavy: bool) -> Self {
        let mut world = PhysicsWorld::new();
        world.set_thread_pool(match executor {
            Executor::Shared(pool) => Some(pool.0.clone()),
            Executor::Current(_, pool) => pool.clone(),
        });
        world.insert(
            RigidBodyBuilder::fixed().translation(Vector::new(0., -1., 0.)),
            ColliderBuilder::cuboid(32., 1., 32.),
        );
        let mut probe = RigidBodyHandle::invalid();
        for i in 0..if heavy { 256 } else { 1 } {
            let pos = Vector::new((i % 16) as f32, 0.5, (i / 16) as f32);
            probe = world
                .insert(
                    RigidBodyBuilder::dynamic()
                        .translation(pos)
                        .can_sleep(false),
                    ColliderBuilder::cuboid(0.48, 0.5, 0.48),
                )
                .0;
        }
        let rig = Skeleton::new((0..32usize).map(|i| i.checked_sub(1)).collect()).unwrap();
        let layout = PoseWorkspace::all(&rig);
        let clip = Clip::new(
            rig,
            vec![LocalPose::new([0.1; 3], [0., 0., 0., 1.], [1.; 3]); 32],
            60.try_into().unwrap(),
            true,
            vec![],
            vec![],
        )
        .unwrap();
        Self {
            world,
            probe,
            clip,
            batches: (0..if heavy { 64 } else { 2 })
                .map(|_| (PoseBatch::new(&layout, 4), 0.))
                .collect(),
            visible: vec![false; if heavy { 4096 } else { 128 }],
        }
    }

    fn frame(&mut self, executor: &mut Executor, physics: bool, jobs: bool) -> [f64; 3] {
        let Self {
            world,
            clip,
            batches,
            visible,
            ..
        } = self;
        let samples = [clip.sample_at(0, 60.try_into().unwrap()); 4];
        let pose = |(batch, tip): &mut (PoseBatch, f32)| {
            let out = black_box(batch.evaluate(&samples).unwrap());
            *tip = out[3].joint(31).unwrap()[3][0];
        };
        let classify = |i: usize, items: &mut [bool]| {
            for (j, v) in items.iter_mut().enumerate() {
                let x = black_box((i + j) % 97) as f32;
                *v = (x - 43.).abs() < 23.;
            }
        };
        let mut times = [0.; 3];
        let [p, a, d] = &mut times;
        let mut simulate = || {
            if physics {
                let t = now();
                world.step();
                *p = now() - t;
            }
        };
        match executor {
            Executor::Current(team, _) => team.scope(|s| {
                if jobs {
                    s.spawn(|| {
                        let t = now();
                        s.scope(|sub| sub.for_each_mut(batches, pose));
                        *a = now() - t;
                    });
                    s.spawn(|| {
                        let t = now();
                        s.scope(|sub| {
                            sub.for_each_chunk_mut(visible, 64.try_into().unwrap(), classify)
                        });
                        *d = now() - t;
                    });
                }
                simulate();
            }),
            Executor::Shared(pool) => pool.run(|| {
                rayon::join(simulate, || {
                    if jobs {
                        rayon::join(
                            || {
                                let t = now();
                                batches.par_iter_mut().for_each(pose);
                                *a = now() - t;
                            },
                            || {
                                let t = now();
                                visible
                                    .par_chunks_mut(64)
                                    .enumerate()
                                    .for_each(|(i, chunk)| classify(i * 64, chunk));
                                *d = now() - t;
                            },
                        );
                    }
                });
            }),
        }
        times
    }
}

/// Columns: host p50/p95, physics/pose/visibility p95 (microseconds), four heap
/// event counts, probe height and visible count. Warmup and result formatting are excluded.
pub fn measure(
    executor: &mut Executor,
    heavy: bool,
    physics: bool,
    jobs: bool,
    pause: impl Fn(),
) -> Vec<f64> {
    let mut scene = Scene::new(executor, heavy);
    for _ in 0..32 {
        scene.frame(executor, physics, jobs);
    }
    let mut samples = [[0.; 96]; 4];
    let start = HeapSnapshot::now();
    for i in 0..96 {
        pause();
        let t = now();
        let stages = scene.frame(executor, physics, jobs);
        samples[0][i] = now() - t;
        for (dst, value) in samples[1..].iter_mut().zip(stages) {
            dst[i] = value;
        }
    }
    let heap = HeapSnapshot::now().since(start);
    assert!(!heap.overflowed);
    for (_, tip) in &scene.batches {
        let expected = if jobs { 3.2 } else { 0. };
        assert!((tip - expected).abs() < 0.00001);
    }
    assert!(
        scene
            .visible
            .iter()
            .enumerate()
            .all(|(i, v)| *v == (jobs && ((i % 97) as f32 - 43.).abs() < 23.))
    );
    for (_, body) in scene.world.bodies.iter().filter(|(_, b)| b.is_dynamic()) {
        assert!((body.translation().y - 0.5).abs() < 0.002);
    }
    for row in &mut samples {
        row.sort_unstable_by(f64::total_cmp);
    }
    let mut out = vec![samples[0][47]];
    out.extend(samples.iter().map(|row| row[91]));
    out.extend(heap.events.iter().map(|event| event.calls as f64));
    out.push(scene.world.bodies[scene.probe].translation().y as f64);
    out.push(scene.visible.iter().filter(|v| **v).count() as f64);
    out
}

/// Verify worker identity outside the timed window, including an external call
/// to Rapier and a nested run. Isolate the allocator cost of generic scoped spawn.
pub fn verify(executor: &mut Executor) -> [u64; 4] {
    use std::sync::Mutex;
    if let Executor::Shared(jobs) = executor {
        struct Hooks {
            ids: Vec<std::thread::ThreadId>,
            observations: Mutex<usize>,
        }
        impl PhysicsHooks for Hooks {
            fn filter_contact_pair(&self, _: &PairFilterContext) -> Option<SolverFlags> {
                assert!(self.ids.contains(&std::thread::current().id()));
                *self.observations.lock().unwrap() += 1;
                Some(SolverFlags::COMPUTE_RIGID_IMPULSES)
            }
        }
        let ids = jobs.0.broadcast(|_| std::thread::current().id());
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            jobs.0.current_num_threads()
        );
        let hooks = Hooks {
            ids,
            observations: Mutex::new(0),
        };
        let nested = jobs.clone();
        jobs.run(|| nested.run(|| assert!(hooks.ids.contains(&std::thread::current().id()))));
        let mut scene = Scene::new(executor, true);
        for (_, collider) in scene.world.colliders.iter_mut() {
            collider.set_active_hooks(ActiveHooks::FILTER_CONTACT_PAIRS);
        }
        scene.world.step_with_events(&hooks, &());
        assert!(*hooks.observations.lock().unwrap() > 0);
    }
    let mut scoped = || {
        let mut a = 0;
        let mut b = 0;
        match executor {
            Executor::Current(team, _) => team.scope(|s| {
                s.spawn(|| a = 1);
                s.spawn(|| b = 2);
            }),
            Executor::Shared(jobs) => jobs.run(|| {
                rayon::scope(|s| {
                    s.spawn(|_| a = 1);
                    s.spawn(|_| b = 2);
                })
            }),
        }
        assert_eq!(a + b, 3);
    };
    for _ in 0..32 {
        scoped();
    }
    let start = HeapSnapshot::now();
    for _ in 0..96 {
        scoped();
    }
    HeapSnapshot::now().since(start).events.map(|e| e.calls)
}

/// Time the shipping game's headless fixed update, including entry from the
/// host. Compare its final public snapshot to a replay on the host.
pub fn survivor_frame(executor: &Executor, late: bool) -> Vec<f64> {
    use survivor::{Arena, Game, Input, Scenario, Upgrade};
    let scenario = if late {
        Scenario::Endgame
    } else {
        Scenario::Early
    };
    let new_game = || Game::new(Arena::default(), scenario, 506);
    let input = Input {
        movement: [1., 0.].into(),
        upgrade: Some(Upgrade::Rapid),
        restart: true,
        ..Input::default()
    };
    let mut game = new_game();
    let mut frame = || {
        let mut step = || black_box(game.step(input));
        match executor {
            Executor::Shared(jobs) => jobs.run(step),
            Executor::Current(_, Some(pool)) => pool.install(step),
            Executor::Current(_, None) => step(),
        }
    };
    for _ in 0..32 {
        frame();
    }
    let mut samples = [0.; 96];
    let start = HeapSnapshot::now();
    for t in &mut samples {
        let clock = now();
        frame();
        *t = now() - clock;
    }
    let heap = HeapSnapshot::now().since(start);
    let mut reference = new_game();
    for _ in 0..128 {
        reference.step(input);
    }
    assert_eq!(reference.snapshot(), game.snapshot());
    samples.sort_unstable_by(f64::total_cmp);
    let mut result = vec![samples[47], samples[91]];
    result.extend(heap.events.map(|e| e.calls as f64));
    result
}

//! PoseBatch latency and allocator checks. Pass --idle to sleep between samples.
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use glam::Quat;
    use sashiko_animation::{Clip, LocalPose, Playback, PoseBatch, PoseWorkspace, Skeleton};
    use sashiko_jobs::JobSystem;
    use sashiko_renderer_core::measurement::{CountingAllocator, Snapshot};
    use std::hint::black_box;
    use std::time::{Duration, Instant};
    #[global_allocator]
    static HEAP: CountingAllocator<std::alloc::System> = CountingAllocator::new(std::alloc::System);
    let idle = std::env::args().any(|a| a == "--idle");
    for (joints, count) in [(16usize, 8usize), (64, 256)] {
        let rig = Skeleton::new((0..joints).map(|i| i.checked_sub(1)).collect()).unwrap();
        let rate = 60.try_into().unwrap();
        let clip = |angle| {
            Clip::new(
                rig.clone(),
                vec![
                    LocalPose::new([1.; 3], Quat::from_rotation_z(angle).to_array(), [1.; 3]);
                    joints
                ],
                rate,
                true,
                vec![],
                vec![],
            )
            .unwrap()
        };
        let source = clip(0.02);
        let target = clip(1.5);
        let mut player = Playback::new(&source, rate);
        player.crossfade(&target, 8.try_into().unwrap()).unwrap();
        player.advance_to(4, &mut []).unwrap();
        for skew in [false, true] {
            let samples = (0..count)
                .map(|i| {
                    if skew && i < count / 4 {
                        player.sample()
                    } else {
                        source.sample_at(0, rate)
                    }
                })
                .collect::<Vec<_>>();
            for n in [1, 2, 4, 8] {
                let mut jobs = JobSystem::new(n.try_into().unwrap()).unwrap();
                let mut batch = PoseBatch::new(&PoseWorkspace::all(&rig), count);
                let mut frame = || {
                    let poses = if n == 1 {
                        batch.evaluate(&samples).unwrap()
                    } else {
                        jobs.scope(|s| batch.evaluate_scoped(&samples, s)).unwrap()
                    };
                    black_box(poses);
                };
                for _ in 0..32 {
                    frame();
                }
                let before = Snapshot::now();
                let mut times = [Duration::ZERO; 64];
                for t in &mut times {
                    if idle {
                        std::thread::sleep(Duration::from_millis(16));
                    }
                    let start = Instant::now();
                    frame();
                    *t = start.elapsed();
                }
                let delta = Snapshot::now().since(before);
                assert!(delta.is_allocation_free(), "{delta:?}");
                times.sort_unstable();
                println!(
                    "{joints}j {count}p skew={skew} n={n}: {:?} / {:?}",
                    times[31], times[60]
                );
                #[cfg(feature = "scheduler-metrics")]
                println!("{:?}", jobs.scheduler_counts());
            }
        }
    }
}
#[cfg(target_arch = "wasm32")]
fn main() {}

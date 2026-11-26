//! The test figure has a root, a leg and a hand. Each limb owns a triangle.
//! Four cooked clips and their gameplay tracks are shared by CPU replay and both native renderers.
use sashiko::animation::{
    AnimatedModel, AnimationError, CharacterCatalog, JointMask, LocalBuffer, PaletteRequest,
    PoseWorkspace, character::*,
};
use sashiko::jobs::JobSystem;
use sashiko_renderer::pipeline::PipelineResult;
use std::{num::NonZeroU32, path::Path, sync::Arc};

const RELOAD: u32 = 10;
const DODGE: u32 = 20;
const EMPTY: Notification = Notification::MovementMarker(u32::MAX);
fn rate() -> NonZeroU32 {
    NonZeroU32::new(60).unwrap()
}
fn catalog() -> PipelineResult<CharacterCatalog> {
    CharacterCatalog::open(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/character_actions/catalog.json"),
    )
}
fn character(c: &CharacterCatalog, model: Option<&AnimatedModel>) -> PipelineResult<Character> {
    let timing = |name| c.timeline("figure", name)?.prepare();
    let gaits = Gaits::new(timing("walk")?, Some(timing("jog")?), 2., 4., rate())?;
    let mut actions = vec![];
    for (id, name, slot, priority, weights) in [
        (RELOAD, "reload", Slot::Upper, 1, [0., 0., 1.]),
        (DODGE, "dodge", Slot::Full, 2, [1.; 3]),
    ] {
        let clip = if let Some(model) = model {
            let clip = Arc::new(model.prepare_pose_clip(name)?);
            let mask = JointMask::new(clip.skeleton(), weights.into())?;
            ActionClip::Posed { clip, mask }
        } else {
            ActionClip::Timeline(timing(name)?)
        };
        actions.push(Action {
            id,
            slot,
            priority,
            clip,
            interruptible: id == RELOAD,
            fade_ticks: 3,
        });
    }
    Ok(Character::new(
        Locomotion::new(gaits),
        ActionSet::new(actions)?,
    ))
}
fn requests(tick: u64) -> &'static [u32] {
    match tick {
        2 => &[RELOAD, RELOAD],
        3 => &[DODGE],
        4 => &[RELOAD, DODGE],
        9 => &[RELOAD],
        _ => &[],
    }
}

#[test]
fn interrupted_reload_matches_headless_retry_and_repeated_display() -> PipelineResult<()> {
    let c = catalog()?;
    let mut replay = character(&c, None)?;
    let model = c.load(c.id("figure").unwrap())?;
    let mut shown = character(&c, Some(&model))?;
    let walk = model.prepare_pose_clip("walk")?;
    let jog = model.prepare_pose_clip("jog")?;
    let mut local = LocalBuffer::new(walk.skeleton());
    let mut full = PoseWorkspace::all(walk.skeleton());
    let mut hand = PoseWorkspace::subset(walk.skeleton(), &[2])?;
    let mut gait_pose = full.clone();
    let mut batch = model.prepare_palette_batch(&[], 1, 1)?;
    let jobs = JobSystem::inline().into();
    let mut translation = 0.;
    let mut reload = None;
    let mut loaded = vec![];
    for tick in 1u64..=16 {
        let mut a = [EMPTY; 8];
        let mut b = a;
        let expected = replay.advance(tick, 4., requests(tick), &mut a)?;
        if expected.notifications > 0 {
            let before = shown.locomotion().sample();
            assert_eq!(
                shown
                    .advance(
                        tick,
                        4.,
                        requests(tick),
                        &mut b[..expected.notifications - 1]
                    )
                    .unwrap_err(),
                AnimationError::EventCapacity {
                    required: expected.notifications as u128
                }
            );
            assert_eq!(shown.tick(), tick - 1);
            assert_eq!(shown.locomotion().sample(), before);
            assert_eq!(b, [EMPTY; 8]);
        }
        let actual = shown.advance(tick, 4., requests(tick), &mut b)?;
        assert_eq!(actual, expected);
        assert_eq!(a, b);
        assert_eq!(shown.locomotion().sample(), replay.locomotion().sample());
        assert_eq!(shown.locomotion().sample().1, (tick % 4) as f64 / 4.);
        let events = &a[..actual.notifications];
        if tick == 2 {
            let [
                Notification::Started(RELOAD, h),
                Notification::Rejected(RELOAD, Rejection::Priority),
            ] = events
            else {
                panic!("{events:?}")
            };
            reload = Some(*h);
        } else if tick == 3 {
            assert!(
                matches!(events, [Notification::Interrupted(h), Notification::Started(DODGE,_)] if Some(*h)==reload)
            );
        } else if tick == 4 {
            assert_eq!(
                events,
                &[
                    Notification::Rejected(RELOAD, Rejection::FullBodyActive),
                    Notification::Rejected(DODGE, Rejection::Protected)
                ]
            );
        }
        assert_eq!(
            events.contains(&Notification::MovementMarker(1)),
            tick % 2 == 1 && !(3..=6).contains(&tick)
        );
        for e in events {
            if let Notification::ActionMarker(handle, 7) = e {
                assert_ne!(Some(*handle), reload);
                loaded.push(tick);
            }
        }
        assert_eq!(
            actual.translation.x,
            if (3..=6).contains(&tick) { 0.25 } else { 0. }
        );
        translation += actual.translation.x;
        for _ in 0..2 {
            shown.compose(&walk, Some(&jog), &mut local)?;
            local.sample().evaluate(&mut full)?;
            local.sample().evaluate(&mut hand)?;
            assert_eq!(hand.joint(2), full.joint(2));
            assert!(hand.joint(1).is_none());
            if tick == 2 || tick >= 9 {
                jog.sample_phase(shown.locomotion().sample().1)?
                    .evaluate(&mut gait_pose)?;
                assert_eq!(gait_pose.joint(1), full.joint(1));
                if tick == 2 {
                    assert_ne!(gait_pose.joint(2), full.joint(2));
                }
            }
            let palette = batch
                .evaluate(
                    &[PaletteRequest::Sample(0)],
                    &[local.sample()],
                    rate(),
                    &jobs,
                )?
                .next()
                .unwrap();
            for (joint, matrix) in palette.iter().enumerate() {
                assert_eq!(
                    matrix.columns(),
                    full.joint(joint).unwrap().to_cols_array_2d()
                );
            }
        }
        assert!(shown.advance(tick, 4., &[], &mut b).is_err());
    }
    assert_eq!(translation, 1.);
    assert_eq!(loaded, [11]);
    Ok(())
}

#[test]
fn stable_character_order_across_worker_counts() -> PipelineResult<()> {
    type TraceEntry = (u64, usize, Vec<Notification>, [f64; 3]);
    fn run(workers: usize) -> PipelineResult<Vec<TraceEntry>> {
        let c = catalog()?;
        let mut team = JobSystem::new(workers.try_into()?)?;
        let mut actors = (0..12)
            .map(|id| Ok((id, character(&c, None)?, [EMPTY; 8], 0, [0.; 3])))
            .collect::<PipelineResult<Vec<_>>>()?;
        let mut trace = Vec::new();
        for tick in 1u64..=16 {
            team.scope(|s| {
                s.for_each_mut(&mut actors, |(id, actor, out, len, delta)| {
                    let phase = tick.saturating_sub((*id % 3) as u64);
                    let r = actor.advance(tick, 4., requests(phase), out).unwrap();
                    *len = r.notifications;
                    *delta = r.translation.to_array();
                })
            });
            trace.extend(
                actors
                    .iter()
                    .map(|(id, _, out, len, d)| (tick, *id, out[..*len].to_vec(), *d)),
            );
        }
        Ok(trace)
    }
    assert_eq!(run(1)?, run(4)?);
    Ok(())
}

#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
#[path = "character_actions/render.rs"]
mod render;

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static HEAP: sashiko_renderer::core::measurement::CountingAllocator<std::alloc::System> =
    sashiko_renderer::core::measurement::CountingAllocator::new(std::alloc::System);

#[cfg(feature = "memory-profiling")]
#[test]
#[ignore = "process-wide allocation counters require isolated execution"]
fn warmed_action_ticks_are_allocation_free() -> PipelineResult<()> {
    use sashiko_renderer::core::measurement::Snapshot;
    let c = catalog()?;
    let model = c.load(c.id("figure").unwrap())?;
    let mut actor = character(&c, Some(&model))?;
    let walk = model.prepare_pose_clip("walk")?;
    let jog = model.prepare_pose_clip("jog")?;
    let mut local = LocalBuffer::new(walk.skeleton());
    let mut batch = model.prepare_palette_batch(&[], 1, 1)?;
    let jobs = JobSystem::inline().into();
    let mut cycle = || -> PipelineResult<()> {
        for step in 1..=16 {
            let tick = actor.tick() + 1;
            std::hint::black_box(actor.advance(tick, 4., requests(step), &mut [EMPTY; 8])?);
            actor.compose(&walk, Some(&jog), &mut local)?;
            let palette = batch
                .evaluate(
                    &[PaletteRequest::Sample(0)],
                    &[local.sample()],
                    rate(),
                    &jobs,
                )?
                .next()
                .unwrap();
            std::hint::black_box(palette);
        }
        Ok(())
    };
    cycle()?;
    let start = Snapshot::now();
    for _ in 0..32 {
        cycle()?;
    }
    let delta = Snapshot::now().since(start);
    assert!(delta.is_allocation_free(), "{delta:?}");
    Ok(())
}

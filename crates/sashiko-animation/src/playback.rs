use super::*;
use std::num::NonZeroU64;

/// Displacement in the clip's root coordinate system and initialized event slots.
#[derive(Debug)]
pub struct Advance {
    pub translation: DVec3,
    pub event_count: usize,
}

#[derive(Clone, Copy)]
struct Track<'a> {
    clip: &'a Clip,
    start: u64,
}

impl Track<'_> {
    fn elapsed(self, tick: u64) -> u64 {
        tick - self.start
    }
    fn event_cursor(self, tick: u64, rate: NonZeroU32) -> (u128, usize) {
        let clip = self.clip;
        let frame = clip.frame(self.elapsed(tick), rate);
        let count = clip.frames as u128;
        let (cycles, index) = if clip.looping {
            (frame / count, frame % count)
        } else {
            (0, frame.min(count - 1))
        };
        (
            cycles,
            clip.markers.partition_point(|e| e.frame <= index as usize),
        )
    }

    fn displacement(self, from: u64, to: u64, rate: NonZeroU32) -> DVec3 {
        let clip = self.clip;
        if clip.root.is_empty() || from == to {
            return DVec3::ZERO;
        }
        let end = clip.root[clip.frames];
        let cursor = |tick| {
            let units = u128::from(self.elapsed(tick)) * u128::from(clip.hz.get());
            let divisor = u128::from(rate.get());
            let frame = units / divisor;
            if !clip.looping && frame >= clip.frames as u128 {
                return (0, end);
            }
            let cycles = frame / clip.frames as u128;
            let index = (frame % clip.frames as u128) as usize;
            let t = (units % divisor) as f64 / divisor as f64;
            (cycles, clip.root[index].lerp(clip.root[index + 1], t))
        };
        let (a, p) = cursor(from);
        let (b, q) = cursor(to);
        end * ((b - a) as f64) + (q - p)
    }
}

struct Transition<'a> {
    source: Track<'a>,
    duration: NonZeroU64,
}

/// Monotonic simulation clock. Pose evaluation is independent of event consumption.
pub struct Playback<'a> {
    active: Track<'a>,
    transition: Option<Transition<'a>>,
    tick: u64,
    rate: NonZeroU32,
}
impl<'a> Playback<'a> {
    pub fn new(clip: &'a Clip, rate: NonZeroU32) -> Self {
        Self {
            active: Track { clip, start: 0 },
            transition: None,
            tick: 0,
            rate,
        }
    }
    /// Begin the target at zero. Finish the current fade before starting another.
    pub fn crossfade(
        &mut self,
        target: &'a Clip,
        duration: NonZeroU64,
    ) -> Result<(), AnimationError> {
        if self.transition.is_some() || !Arc::ptr_eq(&target.skeleton, &self.active.clip.skeleton) {
            return Err(Invalid("incompatible or overlapping crossfade"));
        }
        self.transition = Some(Transition {
            source: self.active,
            duration,
        });
        self.active = Track {
            clip: target,
            start: self.tick,
        };
        Ok(())
    }

    /// Emit markers in (previous tick, tick]. Insufficient space leaves all state and output intact.
    pub fn advance_to(&mut self, tick: u64, events: &mut [u32]) -> Result<Advance, AnimationError> {
        if tick < self.tick {
            return Err(Invalid("playback cannot advance backwards"));
        }
        let (first_cycle, first) = self.active.event_cursor(self.tick, self.rate);
        let (last_cycle, last) = self.active.event_cursor(tick, self.rate);
        let markers = &self.active.clip.markers;
        let cycles = last_cycle - first_cycle;
        let required = if cycles == 0 {
            (last - first) as u128
        } else {
            (cycles - 1)
                .saturating_mul(markers.len() as u128)
                .saturating_add((markers.len() - first) as u128)
                .saturating_add(last as u128)
        };
        if required > events.len() as u128 {
            return Err(AnimationError::EventCapacity { required });
        }
        let target = self.active;
        let mut delta = target.displacement(self.tick, tick, self.rate);
        if let Some(fade) = &self.transition {
            let fade_end = target.start.saturating_add(fade.duration.get()).min(tick);
            // Integrate the displacement on each fixed step. Blending absolute
            // positions would make a walk-to-idle transition slide backwards.
            if !(target.clip.root.is_empty() && fade.source.clip.root.is_empty()) {
                delta = target.displacement(fade_end, tick, self.rate);
                for step in self.tick..fade_end {
                    let weight = (target.elapsed(step) as f64 + 0.5) / fade.duration.get() as f64;
                    let source_step = fade.source.displacement(step, step + 1, self.rate);
                    let target_step = target.displacement(step, step + 1, self.rate);
                    delta += source_step.lerp(target_step, weight);
                }
            }
            if target.elapsed(tick) >= fade.duration.get() {
                self.transition = None;
            }
        }
        for (dest, marker) in events[..required as usize]
            .iter_mut()
            .zip(markers[first..].iter().chain(markers.iter().cycle()))
        {
            *dest = marker.id;
        }
        self.tick = tick;
        Ok(Advance {
            translation: delta,
            event_count: required as usize,
        })
    }

    pub fn evaluate_pose(&self, out: &mut PoseWorkspace) -> Result<(), AnimationError> {
        let target = self.active.clip;
        let elapsed = self.active.elapsed(self.tick);
        if let Some(fade) = &self.transition {
            let source = fade.source.clip;
            let local = source.pose(fade.source.elapsed(self.tick), self.rate);
            if elapsed == 0 {
                return out.evaluate(source, local, None);
            }
            return out.evaluate(
                source,
                local,
                Some((
                    target.pose(elapsed, self.rate),
                    elapsed as f32 / fade.duration.get() as f32,
                )),
            );
        }
        target.evaluate_at(elapsed, self.rate, out)
    }
}

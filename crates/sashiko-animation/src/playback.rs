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
    fn displacement(self, from: u64, to: u64, rate: NonZeroU32) -> DVec3 {
        self.clip
            .timeline
            .displacement(self.elapsed(from), self.elapsed(to), rate)
    }
}

impl ClipTimeline {
    fn event_cursor(&self, tick: u64, rate: NonZeroU32) -> (u128, usize) {
        let frame = self.frame(tick, rate);
        let count = self.frames as u128;
        let (cycles, index) = if self.looping {
            (frame / count, frame % count)
        } else {
            (0, frame.min(count - 1))
        };
        (
            cycles,
            self.markers.partition_point(|e| e.frame <= index as usize),
        )
    }

    // Markers in (from, to], including any complete loops.
    fn event_range(&self, from: u64, to: u64, rate: NonZeroU32) -> (usize, u128) {
        let (first_cycle, first) = self.event_cursor(from, rate);
        let (last_cycle, last) = self.event_cursor(to, rate);
        let markers = &self.markers;
        let cycles = last_cycle - first_cycle;
        let required = if cycles == 0 {
            (last - first) as u128
        } else {
            (cycles - 1)
                .saturating_mul(markers.len() as u128)
                .saturating_add((markers.len() - first) as u128)
                .saturating_add(last as u128)
        };
        (first, required)
    }
    /// Visit the markers crossed on this single simulation tick, in asset order.
    pub fn events_at(&self, tick: u64, rate: NonZeroU32) -> impl Iterator<Item = u32> + '_ {
        let (first, mut remaining) = self.event_range(tick.saturating_sub(1), tick, rate);
        let mut markers = self.markers[first..]
            .iter()
            .chain(self.markers.iter().cycle());
        std::iter::from_fn(move || {
            remaining = remaining.checked_sub(1)?;
            markers.next().map(|m| m.id)
        })
    }

    fn displacement(&self, from: u64, to: u64, rate: NonZeroU32) -> DVec3 {
        if self.root.is_empty() || from == to {
            return DVec3::ZERO;
        }
        let end = self.root[self.frames];
        let cursor = |tick| {
            let units = u128::from(tick) * u128::from(self.hz.get());
            let divisor = u128::from(rate.get());
            let frame = units / divisor;
            if !self.looping && frame >= self.frames as u128 {
                return (0, end);
            }
            let cycles = frame / self.frames as u128;
            let index = (frame % self.frames as u128) as usize;
            let t = (units % divisor) as f64 / divisor as f64;
            (cycles, self.root[index].lerp(self.root[index + 1], t))
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
        let target = self.active;
        let (first, required) = target.clip.timeline.event_range(
            target.elapsed(self.tick),
            target.elapsed(tick),
            self.rate,
        );
        let markers = &target.clip.timeline.markers;
        if required > events.len() as u128 {
            return Err(AnimationError::EventCapacity { required });
        }
        let mut delta = target.displacement(self.tick, tick, self.rate);
        if let Some(fade) = &self.transition {
            let fade_end = target.start.saturating_add(fade.duration.get()).min(tick);
            // Integrate the displacement on each fixed step. Blending absolute
            // positions would make a walk-to-idle transition slide backwards.
            if !(target.clip.timeline.root.is_empty() && fade.source.clip.timeline.root.is_empty())
            {
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
        self.sample().evaluate(out)
    }
    pub fn sample(&self) -> PoseSample<'a> {
        let target = self.active.clip;
        let elapsed = self.active.elapsed(self.tick);
        if let Some(fade) = &self.transition {
            let source = fade.source.clip;
            let local = source.pose(fade.source.elapsed(self.tick), self.rate);
            if elapsed == 0 {
                return PoseSample {
                    clip: source,
                    local,
                    blend: None,
                };
            }
            return PoseSample {
                clip: source,
                local,
                blend: Some((
                    target.pose(elapsed, self.rate),
                    elapsed as f32 / fade.duration.get() as f32,
                )),
            };
        }
        target.sample_at(elapsed, self.rate)
    }
}

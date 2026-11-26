use glam::DVec3;
use sashiko_animation::{AnimationError, ClipTimeline, PhaseMap};
use std::{num::NonZeroU32, sync::Arc};

/// Immutable timelines and speed thresholds shared by characters.
pub struct Gaits {
    walk: ClipTimeline,
    jog: Option<ClipTimeline>,
    sync: Option<PhaseMap>,
    walk_speed: f32,
    jog_speed: f32,
    rate: NonZeroU32,
}
impl Gaits {
    pub fn new(
        walk: ClipTimeline,
        jog: Option<ClipTimeline>,
        walk_speed: f32,
        jog_speed: f32,
        rate: NonZeroU32,
    ) -> Result<Arc<Self>, AnimationError> {
        if !walk_speed.is_finite()
            || walk_speed < 0.
            || !jog_speed.is_finite()
            || jog_speed <= walk_speed
        {
            return Err(AnimationError::Invalid("invalid gait speed range"));
        }
        let sync = jog.as_ref().and_then(|j| PhaseMap::between(&walk, j));
        Ok(Arc::new(Self {
            walk,
            jog,
            sync,
            walk_speed,
            jog_speed,
            rate,
        }))
    }
}
#[derive(Clone)]
pub struct Locomotion {
    gaits: Arc<Gaits>,
    position: f64,
    compensation: f64,
    weight: f32,
}
/// Authored markers in chronological order on the most recent movement interval.
pub struct Movement<'a> {
    markers: &'a [sashiko_animation::Marker],
    start: usize,
    count: usize,
    pub translation: DVec3,
}
impl Movement<'_> {
    pub fn events(&self) -> impl Iterator<Item = u32> + '_ {
        self.markers
            .iter()
            .cycle()
            .skip(self.start)
            .take(self.count)
            .map(|m| m.id)
    }
    pub fn event_count(&self) -> usize {
        self.count
    }
}
impl Locomotion {
    pub fn new(gaits: Arc<Gaits>) -> Self {
        Self {
            gaits,
            position: 0.,
            compensation: 0.,
            weight: 0.,
        }
    }
    pub fn restarted(&self) -> Self {
        Self::new(self.gaits.clone())
    }
    pub fn replace_tracks(&mut self, other: Self) {
        let phase = self.position / self.gaits.walk.frames() as f64;
        self.gaits = other.gaits;
        self.position = phase * self.gaits.walk.frames() as f64;
        self.compensation = 0.;
        if self.gaits.jog.is_none() {
            self.weight = 0.;
        }
    }
    pub fn tick_rate(&self) -> NonZeroU32 {
        self.gaits.rate
    }
    /// Normalized display positions for both tracks and the walk-to-jog blend.
    pub fn sample(&self) -> (f64, f64, f32) {
        let phase = self.position / self.gaits.walk.frames() as f64;
        let wrap = |p: f64, t: &ClipTimeline| {
            if t.is_looping() {
                p.rem_euclid(1.)
            } else {
                p.clamp(0., 1.)
            }
        };
        let jog_phase = self.gaits.sync.as_ref().map_or(phase, |s| s.map(phase));
        (
            wrap(phase, &self.gaits.walk),
            wrap(
                jog_phase,
                self.gaits.jog.as_ref().unwrap_or(&self.gaits.walk),
            ),
            self.weight,
        )
    }

    /// Advance one fixed tick. Zero speed holds phase and blend weight.
    pub fn advance(&mut self, speed: f32) -> Result<Movement<'_>, AnimationError> {
        if !speed.is_finite() || speed < 0. {
            return Err(AnimationError::Invalid("invalid movement speed"));
        }
        if speed == 0. {
            return Ok(Movement {
                markers: &[],
                start: 0,
                count: 0,
                translation: DVec3::ZERO,
            });
        }
        let walk = &self.gaits.walk;
        let jog = self.gaits.jog.as_ref().unwrap_or(walk);
        let weight = if self.gaits.jog.is_some() {
            ((speed - self.gaits.walk_speed) / (self.gaits.jog_speed - self.gaits.walk_speed))
                .clamp(0., 1.)
        } else {
            0.
        };
        let frames = walk.frames() as f64;
        let walk_rate = f64::from(walk.sample_rate().get());
        let jog_rate = f64::from(jog.sample_rate().get()) * frames / jog.frames() as f64;
        let delta = (walk_rate + (jog_rate - walk_rate) * f64::from(weight))
            / f64::from(self.gaits.rate.get());
        let before = self.position / frames;
        let increment = delta - self.compensation;
        let next = self.position + increment;
        let compensation = (next - self.position) - increment;
        let after = next / frames;
        // Matched contacts are expressed in the walk cycle. With unmatched tracks,
        // use the dominant clip's events for this interval, including at a switch.
        let matched = self.gaits.sync.is_some();
        let track = if matched || weight < 0.5 { walk } else { jog };

        let markers = track.markers();
        let cursor = |phase: f64| {
            // Preserve authored seam events when floating-point integration lands
            // within roundoff of the next cycle (including frame zero).
            let nearest = phase.round();
            let phase = if (phase - nearest).abs() < 8. * f64::EPSILON {
                nearest
            } else {
                phase
            };
            let cycles = if track.is_looping() {
                phase.floor()
            } else {
                0.
            };
            let partial = phase - cycles;
            let index = markers.partition_point(|m| {
                m.frame as f64 / track.frames() as f64 <= partial + 8. * f64::EPSILON
            });
            (cycles as u64, index)
        };
        // At magnitudes beyond exact integer cycles a clock can no longer
        // distinguish authored events. Fail without committing a partial tick.
        if after >= (1u64 << 53) as f64 || after < before {
            return Err(AnimationError::Invalid(
                "movement phase exceeds clock precision",
            ));
        }
        let (a, start) = cursor(before);
        let (b, end) = cursor(after);
        let count = (u128::from(b - a) * markers.len() as u128 + end as u128) - start as u128;
        let count = usize::try_from(count)
            .map_err(|_| AnimationError::EventCapacity { required: count })?;
        let jog_phase = |p| self.gaits.sync.as_ref().map_or(p, |s| s.map(p));
        let translation = walk.phase_displacement(before, after)?.lerp(
            jog.phase_displacement(jog_phase(before), jog_phase(after))?,
            f64::from(weight),
        );
        self.compensation = compensation;
        self.position = next;
        self.weight = weight;
        Ok(Movement {
            markers,
            start,
            count,
            translation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sashiko_animation::Marker;

    fn track(frames: usize, hz: u32, id: u32) -> ClipTimeline {
        ClipTimeline::new(
            frames,
            hz.try_into().unwrap(),
            true,
            vec![
                Marker { frame: 0, id },
                Marker {
                    frame: 1,
                    id: id + 1,
                },
            ],
            (0..=frames).map(|i| DVec3::X * i as f64).collect(),
        )
        .unwrap()
    }

    #[test]
    fn dominant_markers_cross_multiple_loops_in_order_and_root_moves_once() {
        let gaits = Gaits::new(
            track(2, 8, 1),
            Some(track(2, 8, 10)),
            2.,
            4.,
            NonZeroU32::MIN,
        )
        .unwrap();
        let mut player = Locomotion::new(gaits);
        let movement = player.advance(3.).unwrap();
        assert_eq!(
            movement.events().collect::<Vec<_>>(),
            [11, 10, 11, 10, 11, 10, 11, 10]
        );
        assert_eq!(movement.translation, DVec3::X * 8.);
        assert_eq!(player.advance(2.).unwrap().events().next(), Some(2));
    }

    #[test]
    fn changing_models_preserves_normalized_phase_and_resolves_missing_jog() {
        let clock = 4.try_into().unwrap();
        let mut player = Locomotion::new(
            Gaits::new(track(4, 4, 1), Some(track(4, 4, 2)), 2., 4., clock).unwrap(),
        );
        player.advance(4.).unwrap();
        assert_eq!(player.sample(), (0.25, 0.25, 1.));
        player.replace_tracks(Locomotion::new(
            Gaits::new(track(8, 4, 1), None, 2., 4., clock).unwrap(),
        ));
        assert_eq!(player.sample(), (0.25, 0.25, 0.));
        player.advance(4.).unwrap();
        assert_eq!(player.sample(), (0.375, 0.375, 0.));
    }
}

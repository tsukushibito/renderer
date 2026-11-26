use sashiko_animation::{AnimationError, Clip, ClipTimeline, PhaseMap, Skeleton};
use std::{num::NonZeroU32, sync::Arc};

enum Tracks {
    Headless {
        walk: ClipTimeline,
        jog: Option<ClipTimeline>,
    },
    Posed {
        walk: Arc<Clip>,
        jog: Option<Arc<Clip>>,
    },
}
impl Tracks {
    fn timelines(&self) -> (&ClipTimeline, Option<&ClipTimeline>) {
        match self {
            Self::Headless { walk, jog } => (walk, jog.as_ref()),
            Self::Posed { walk, jog } => (walk.timeline(), jog.as_ref().map(|c| c.timeline())),
        }
    }
}

/// Shared walk and optional jog with their speed thresholds.
pub struct Gaits {
    tracks: Tracks,
    sync: Option<PhaseMap>,
    walk_speed: f32,
    jog_speed: f32,
    rate: NonZeroU32,
}
impl Gaits {
    /// Prepare timeline-only gaits for simulation without pose data.
    pub fn from_timelines(
        walk: ClipTimeline,
        jog: Option<ClipTimeline>,
        walk_speed: f32,
        jog_speed: f32,
        rate: NonZeroU32,
    ) -> Result<Arc<Self>, AnimationError> {
        Self::prepare(Tracks::Headless { walk, jog }, walk_speed, jog_speed, rate)
    }

    /// Bind pose clips once. Their own timelines drive markers, root motion and phase.
    pub fn from_clips(
        walk: Arc<Clip>,
        jog: Option<Arc<Clip>>,
        walk_speed: f32,
        jog_speed: f32,
        rate: NonZeroU32,
    ) -> Result<Arc<Self>, AnimationError> {
        if jog
            .as_ref()
            .is_some_and(|j| !Arc::ptr_eq(walk.skeleton(), j.skeleton()))
        {
            return Err(AnimationError::Invalid("gait skeletons differ"));
        }
        Self::prepare(Tracks::Posed { walk, jog }, walk_speed, jog_speed, rate)
    }

    fn prepare(
        tracks: Tracks,
        walk_speed: f32,
        jog_speed: f32,
        rate: NonZeroU32,
    ) -> Result<Arc<Self>, AnimationError> {
        let (walk, jog) = tracks.timelines();
        if !walk_speed.is_finite()
            || walk_speed < 0.
            || !jog_speed.is_finite()
            || jog_speed <= walk_speed
        {
            return Err(AnimationError::Invalid("invalid gait speed range"));
        }
        let sync = jog.and_then(|j| PhaseMap::between(walk, j));
        Ok(Arc::new(Self {
            tracks,
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
    pub translation: [f64; 3],
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
        let phase = self.position / self.gaits.tracks.timelines().0.frames() as f64;
        self.gaits = other.gaits;
        self.position = phase * self.gaits.tracks.timelines().0.frames() as f64;
        self.compensation = 0.;
        if self.gaits.tracks.timelines().1.is_none() {
            self.weight = 0.;
        }
    }
    /// Skeleton used to allocate pose workspaces for this character.
    pub fn skeleton(&self) -> Option<&Arc<Skeleton>> {
        self.clips().map(|(walk, _)| walk.skeleton())
    }
    pub(crate) fn clips(&self) -> Option<(&Clip, Option<&Clip>)> {
        match &self.gaits.tracks {
            Tracks::Headless { .. } => None,
            Tracks::Posed { walk, jog } => Some((walk, jog.as_deref())),
        }
    }
    pub fn tick_rate(&self) -> NonZeroU32 {
        self.gaits.rate
    }
    /// Normalized display positions for both tracks and the walk-to-jog blend.
    pub fn sample(&self) -> (f64, f64, f32) {
        let (walk, jog) = self.gaits.tracks.timelines();
        let phase = self.position / walk.frames() as f64;
        let wrap = |p: f64, t: &ClipTimeline| {
            if t.is_looping() {
                p.rem_euclid(1.)
            } else {
                p.clamp(0., 1.)
            }
        };
        let jog_phase = self.gaits.sync.as_ref().map_or(phase, |s| s.map(phase));
        (
            wrap(phase, walk),
            wrap(jog_phase, jog.unwrap_or(walk)),
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
                translation: [0.; 3],
            });
        }
        let (walk, jog) = self.gaits.tracks.timelines();
        let weight = if jog.is_some() {
            ((speed - self.gaits.walk_speed) / (self.gaits.jog_speed - self.gaits.walk_speed))
                .clamp(0., 1.)
        } else {
            0.
        };
        let jog = jog.unwrap_or(walk);
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
        let walk_delta = walk.phase_displacement(before, after)?;
        let jog_delta = jog.phase_displacement(jog_phase(before), jog_phase(after))?;
        let blend = f64::from(weight);
        let translation =
            std::array::from_fn(|i| walk_delta[i] * (1. - blend) + jog_delta[i] * blend);
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
            (0..=frames).map(|i| [i as f64, 0., 0.]).collect(),
        )
        .unwrap()
    }

    #[test]
    fn dominant_markers_cross_multiple_loops_in_order_and_root_moves_once() {
        let gaits = Gaits::from_timelines(
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
        assert_eq!(movement.translation, [8., 0., 0.]);
        assert_eq!(player.advance(2.).unwrap().events().next(), Some(2));
    }

    #[test]
    fn gait_clips_must_share_the_same_skeleton() {
        let clip = || {
            let rig = Skeleton::new(vec![None]).unwrap();
            Arc::new(
                Clip::from_timeline(
                    rig,
                    vec![sashiko_animation::LocalPose::new([0.; 3], [0., 0., 0., 1.], [1.; 3]); 2],
                    track(2, 8, 1),
                )
                .unwrap(),
            )
        };
        let walk = clip();
        assert!(
            Gaits::from_clips(walk.clone(), Some(walk.clone()), 2., 4., NonZeroU32::MIN).is_ok()
        );
        assert!(Gaits::from_clips(walk, Some(clip()), 2., 4., NonZeroU32::MIN).is_err());
    }

    #[test]
    fn changing_models_preserves_normalized_phase_and_resolves_missing_jog() {
        let clock = 4.try_into().unwrap();
        let mut player = Locomotion::new(
            Gaits::from_timelines(track(4, 4, 1), Some(track(4, 4, 2)), 2., 4., clock).unwrap(),
        );
        player.advance(4.).unwrap();
        assert_eq!(player.sample(), (0.25, 0.25, 1.));
        player.replace_tracks(Locomotion::new(
            Gaits::from_timelines(track(8, 4, 1), None, 2., 4., clock).unwrap(),
        ));
        assert_eq!(player.sample(), (0.25, 0.25, 0.));
        player.advance(4.).unwrap();
        assert_eq!(player.sample(), (0.375, 0.375, 0.));
    }
}

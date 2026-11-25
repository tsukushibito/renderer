//! Fixed-tick player locomotion. Rendering can seek the current pose independently.
use sashiko::animation::{CharacterCatalog, ClipTimeline, PhaseMap, TimelineAsset};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
pub fn catalog_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let relative = Path::new("assets/survivor/characters/catalog.json");
    let packaged = exe
        .parent()
        .ok_or("executable directory unavailable")?
        .join(relative);
    if exe.with_file_name("sashiko-package.json").try_exists()? || packaged.try_exists()? {
        return Ok(packaged);
    }
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative))
}

struct Gaits {
    walk: ClipTimeline,
    jog: Option<ClipTimeline>,
    sync: Option<PhaseMap>,
}

pub struct Locomotion {
    gaits: Arc<Gaits>,
    // Integrated in walk-frame units so native-rate sampling lands on exact frames.
    position: f64,
    compensation: f64,
    weight: f32,
}

impl Default for Locomotion {
    fn default() -> Self {
        static BUNDLED: OnceLock<Arc<Gaits>> = OnceLock::new();
        let gaits = BUNDLED.get_or_init(|| {
            let catalog = CharacterCatalog::parse(
                Path::new("assets/survivor/characters/catalog.json"),
                include_bytes!("../../../assets/survivor/characters/catalog.json"),
            )
            .expect("bundled character catalog");
            Self::from_catalog(&catalog, "primary")
                .expect("bundled locomotion")
                .gaits
        });
        Self {
            gaits: gaits.clone(),
            position: 0.,
            compensation: 0.,
            weight: 0.,
        }
    }
}
impl Locomotion {
    pub fn from_catalog(
        catalog: &CharacterCatalog,
        model: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        LocomotionClips::load(catalog, model)?.play()
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct LocomotionClips {
    walk: TimelineAsset,
    jog: Option<TimelineAsset>,
}
impl LocomotionClips {
    pub fn load(
        catalog: &CharacterCatalog,
        model: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let walk = catalog.timeline(model, "walk")?;
        let jog = catalog
            .has_clip(model, "jog")
            .then(|| catalog.timeline(model, "jog"))
            .transpose()?;
        Ok(Self { walk, jog })
    }
    pub fn play(&self) -> Result<Locomotion, Box<dyn std::error::Error>> {
        let walk = self.walk.prepare()?;
        let jog = self.jog.as_ref().map(TimelineAsset::prepare).transpose()?;
        let sync = jog.as_ref().and_then(|j| PhaseMap::between(&walk, j));
        Ok(Locomotion {
            gaits: Arc::new(Gaits { walk, jog, sync }),
            position: 0.,
            compensation: 0.,
            weight: 0.,
        })
    }
}

impl Locomotion {
    #[cfg(all(
        windows,
        feature = "embed",
        feature = "presentation",
        any(feature = "d3d12", feature = "vulkan")
    ))]
    pub(crate) fn replace_tracks(&mut self, other: Self) {
        let phase = self.position / self.gaits.walk.frames() as f64;
        self.gaits = other.gaits;
        self.position = phase * self.gaits.walk.frames() as f64;
        self.compensation = 0.;
        if self.gaits.jog.is_none() {
            self.weight = 0.;
        }
    }

    pub(crate) fn restarted(&self) -> Self {
        Self {
            gaits: self.gaits.clone(),
            position: 0.,
            compensation: 0.,
            weight: 0.,
        }
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

    pub(crate) fn advance(&mut self, speed: f32) -> usize {
        if speed < 0.01 {
            return 0;
        }
        let walk = &self.gaits.walk;
        let jog = self.gaits.jog.as_ref().unwrap_or(walk);
        self.weight = if self.gaits.jog.is_some() {
            ((speed - 2.) / 2.).clamp(0., 1.)
        } else {
            0.
        };
        let frames = walk.frames() as f64;
        let walk_rate = f64::from(walk.sample_rate().get());
        let jog_rate = f64::from(jog.sample_rate().get()) * frames / jog.frames() as f64;
        let delta = (walk_rate + (jog_rate - walk_rate) * f64::from(self.weight))
            / f64::from(crate::TICK_HZ);
        let before = self.position / frames;
        let increment = delta - self.compensation;
        let next = self.position + increment;
        self.compensation = (next - self.position) - increment;
        self.position = next;
        let after = next / frames;
        // Matched contacts are expressed in the walk cycle. With unmatched tracks,
        // use the dominant clip's events for this interval, including at a switch.
        let matched = self.gaits.sync.is_some();
        let track = if matched || self.weight < 0.5 {
            walk
        } else {
            jog
        };
        track
            .markers()
            .iter()
            .filter(|m| m.id == 1)
            .map(|m| {
                let contact = m.frame as f64 / track.frames() as f64;
                let crossings = |phase: f64| {
                    let distance = phase - contact;
                    // Round only arithmetic noise at an authored contact boundary.
                    let value = if (distance - distance.round()).abs() < 8. * f64::EPSILON {
                        distance.round()
                    } else {
                        distance
                    };
                    if track.is_looping() {
                        value.floor()
                    } else {
                        f64::from(value >= 0.)
                    }
                };
                (crossings(after) - crossings(before)) as usize
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_tracks_reproduce_contacts_and_phase_through_switches() {
        let catalog = CharacterCatalog::open(&catalog_path().unwrap()).unwrap();
        let clips = LocomotionClips::load(&catalog, "primary").unwrap();
        let mut live = clips.play().unwrap();
        let copy: LocomotionClips =
            serde_json::from_slice(&serde_json::to_vec(&clips).unwrap()).unwrap();
        let mut replay = copy.play().unwrap();
        for tick in 1..=160 {
            let speed = match tick {
                1..=19 => 2.,
                20..=34 => 3.,
                35..=40 => 0.,
                41..=80 => 4.,
                _ => 2.,
            };
            let old = live.sample().0;
            let count = live.advance(speed);
            assert_eq!(count, replay.advance(speed));
            let (phase, jog, w) = live.sample();
            assert_eq!(live.sample(), replay.sample());
            assert!((phase - jog).abs() < 1e-12);
            let crossed = |x| {
                if phase >= old {
                    old < x && x <= phase
                } else {
                    x > old || x <= phase
                }
            };
            assert_eq!(
                count,
                [0.25, 0.75].into_iter().filter(|&x| crossed(x)).count()
            );
            if (20..=34).contains(&tick) {
                assert_eq!(w, 0.5);
            }
        }
        let mut only_walk = LocomotionClips {
            walk: clips.walk,
            jog: None,
        }
        .play()
        .unwrap();
        for tick in 1..=80 {
            assert_eq!(only_walk.advance(4.), usize::from(tick == 20 || tick == 60));
        }
        assert_eq!(only_walk.sample().2, 0.);
    }
    #[test]
    fn independent_clocks_emit_authored_contacts_only_on_moving_ticks() {
        let mut walk = Locomotion::default();
        let mut run = walk.restarted();
        for step in 1..=80 {
            assert_eq!(walk.advance(2.), usize::from(matches!(step, 20 | 60)));
            assert_eq!(run.advance(4.), usize::from(matches!(step, 14 | 42 | 70)));
            assert_eq!(walk.advance(0.), 0);
        }
        let held = walk.sample();
        walk.advance(0.);
        assert_eq!(walk.sample(), held);
        assert_eq!(walk.advance(4.), 0);
        assert_eq!(walk.sample().2, 1.);
        assert!((walk.sample().0 - 1. / 56.).abs() < 1e-12);
        assert!((run.sample().1 - 24. / 56.).abs() < 1e-12);
    }
}

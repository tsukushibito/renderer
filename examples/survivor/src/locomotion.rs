//! Fixed-tick player locomotion. Rendering can seek the current pose independently.
use sashiko::animation::{CharacterCatalog, TimelineAsset, character};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
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

pub struct Locomotion(character::Locomotion);

impl Default for Locomotion {
    fn default() -> Self {
        static BUNDLED: OnceLock<Locomotion> = OnceLock::new();
        let gaits = BUNDLED.get_or_init(|| {
            let catalog = CharacterCatalog::parse(
                Path::new("assets/survivor/characters/catalog.json"),
                include_bytes!("../../../assets/survivor/characters/catalog.json"),
            )
            .expect("bundled character catalog");
            Self::from_catalog(&catalog, "primary").expect("bundled locomotion")
        });
        gaits.restarted()
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
        Ok(Locomotion(character::Locomotion::new(
            character::Gaits::from_timelines(
                walk,
                jog,
                2.,
                4.,
                crate::TICK_HZ.try_into().unwrap(),
            )?,
        )))
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
        self.0.replace_tracks(other.0);
    }
    pub(crate) fn restarted(&self) -> Self {
        Self(self.0.restarted())
    }
    pub fn sample(&self) -> (f64, f64, f32) {
        self.0.sample()
    }
    pub(crate) fn advance(&mut self, speed: f32) -> usize {
        self.0
            .advance(if speed < 0.01 { 0. } else { speed })
            .expect("finite simulation speed")
            .events()
            .filter(|&id| id == 1)
            .count()
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

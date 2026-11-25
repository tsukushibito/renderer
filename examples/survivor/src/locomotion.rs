//! Fixed-tick player locomotion. Rendering can seek the current pose independently.
use sashiko::animation::{CharacterCatalog, ClipTimeline, TimelineAsset};
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
}

pub struct Locomotion {
    gaits: Arc<Gaits>,
    jogging: bool,
    tick: u64,
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
            jogging: false,
            tick: 0,
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
        Ok(Locomotion {
            gaits: Arc::new(Gaits { walk, jog }),
            jogging: false,
            tick: 0,
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
        self.gaits = other.gaits;
        if self.jogging && self.gaits.jog.is_none() {
            self.jogging = false;
            self.tick = 0;
        }
    }

    pub(crate) fn restarted(&self) -> Self {
        Self {
            gaits: self.gaits.clone(),
            jogging: false,
            tick: 0,
        }
    }
    pub fn jogging(&self) -> bool {
        self.jogging
    }
    pub fn tick(&self) -> u64 {
        self.tick
    }
    pub(crate) fn advance(&mut self, speed: f32) -> usize {
        if speed < 0.01 {
            return 0;
        }
        let jog = self.gaits.jog.as_ref().filter(|_| speed > 3.0);
        if self.jogging != jog.is_some() {
            self.tick = 0;
            self.jogging = jog.is_some();
        }
        self.tick += 1;
        jog.unwrap_or(&self.gaits.walk)
            .events_at(self.tick, crate::TICK_HZ.try_into().unwrap())
            .filter(|&id| id == 1)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_clocks_emit_authored_contacts_only_on_moving_ticks() {
        let mut walk = Locomotion::default();
        let mut run = walk.restarted();
        for step in 1..=80 {
            assert_eq!(walk.advance(2.), usize::from(matches!(step, 20 | 60)));
            assert_eq!(run.advance(4.), usize::from(matches!(step, 14 | 42 | 70)));
            assert_eq!(walk.advance(0.), 0);
        }
        assert_eq!(walk.tick(), 80);
        assert_eq!(walk.advance(4.), 0);
        assert!(walk.jogging());
        assert_eq!(walk.tick(), 1);
        assert_eq!(run.tick(), 80);
    }
}

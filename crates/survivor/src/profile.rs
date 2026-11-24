//! Survivor owns the unlock rule and the user's choices. The engine owns the durable slot.
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use sashiko::save::{SaveData, SaveSlot, user_directory};
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf};

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct Preferences {
    pub alternate_suit_unlocked: bool,
    pub width: u32,
    pub height: u32,
    pub volume: f32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            alternate_suit_unlocked: false,
            width: 1920,
            height: 1080,
            volume: 1.0,
        }
    }
}
impl SaveData for Preferences {
    const VERSION: u32 = 1;
    fn validate(&self) -> io::Result<()> {
        if self.width == 0 || self.height == 0 || !(0.0..=1.0).contains(&self.volume) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid display extent or audio volume",
            ));
        }
        Ok(())
    }
}

pub struct Player {
    slot: Option<SaveSlot>,
    pub prefs: Preferences,
}

impl Player {
    pub fn arguments(cmd: Command) -> Command {
        cmd.arg(
            Arg::new("user-dir")
                .long("user-dir")
                .value_parser(value_parser!(PathBuf))
                .help("Read and update an isolated survivor profile here"),
        )
        .arg(
            Arg::new("profile-status")
                .long("profile-status")
                .action(ArgAction::SetTrue)
                .help("Print saved unlocks and settings as JSON"),
        )
        .arg(
            Arg::new("volume")
                .long("volume")
                .value_parser(value_parser!(f32))
                .help("Master volume from 0 to 1"),
        )
    }

    pub fn load(args: &ArgMatches) -> io::Result<Self> {
        let isolated = args.get_flag("headless")
            || ["frames", "record", "replay"]
                .iter()
                .any(|name| args.contains_id(name));
        let directory = match args.get_one::<PathBuf>("user-dir") {
            Some(p) => Some(p.clone()),
            None if args.get_flag("profile-status") || !isolated => {
                Some(user_directory("survivor")?)
            }
            None => None,
        };
        let slot = directory.map(SaveSlot::new);
        let mut prefs = slot
            .as_ref()
            .map(SaveSlot::read)
            .transpose()?
            .unwrap_or_default();
        let (width, height, volume) = (
            args.get_one::<u32>("width"),
            args.get_one::<u32>("height"),
            args.get_one::<f32>("volume"),
        );
        if width.is_some() || height.is_some() || volume.is_some() {
            let edit = |p: &mut Preferences| {
                if let Some(v) = width {
                    p.width = *v;
                }
                if let Some(v) = height {
                    p.height = *v;
                }
                if let Some(v) = volume {
                    p.volume = *v;
                }
            };
            if let Some(slot) = &slot {
                prefs = slot.update(edit)?;
            } else {
                edit(&mut prefs);
                prefs.validate()?;
            }
        }
        Ok(Self { slot, prefs })
    }

    /// The alternate lab suit is earned on the first confirmed kill. Death or a
    /// restart in this session cannot erase progress after the write succeeds.
    pub fn observe(&mut self, kills: u32) -> io::Result<()> {
        if kills > 0
            && !self.prefs.alternate_suit_unlocked
            && let Some(slot) = &self.slot
        {
            self.prefs =
                slot.update(|prefs: &mut Preferences| prefs.alternate_suit_unlocked = true)?;
        }
        Ok(())
    }
}

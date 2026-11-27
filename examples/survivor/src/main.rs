#[cfg(feature = "diagnostics")]
mod diagnostics;
mod profile;

use clap::{Arg, ArgAction, Command, value_parser};
use sashiko::{App, app::Replay};
use std::num::NonZeroU32;
use std::{error::Error, fs, path::PathBuf};
use survivor::{Arena, Game, Input, Scenario, TICK_HZ, benchmark_input};

fn rate() -> NonZeroU32 {
    NonZeroU32::new(TICK_HZ).unwrap()
}

use survivor::RecordedVisual;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct RunStart {
    arena: serde_json::Value,
    scenario: Scenario,
    seed: u64,
    rules_version: u32,
    #[serde(default)]
    visual: Option<RecordedVisual>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    locomotion: Option<survivor::LocomotionClips>,
}

impl RunStart {
    fn game(&self, workers: sashiko::physics::PhysicsWorkers) -> Result<Game, String> {
        Ok(Game::with_physics_workers(
            Arena::from_value(self.arena.clone())?,
            self.scenario,
            self.seed,
            workers,
        ))
    }
}

#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
mod window;

fn arguments() -> Command {
    let flag = |name| Arg::new(name).long(name).action(ArgAction::SetTrue);
    let path = |name| {
        Arg::new(name)
            .long(name)
            .value_parser(value_parser!(PathBuf))
    };
    let command = profile::Player::arguments(sashiko::cli::game_command(Command::new("survivor")));
    #[cfg(feature = "diagnostics")]
    let command = command
        .arg(
            path("debug-dir")
                .conflicts_with_all(["headless", "replay-from"])
                .help("Enable play diagnostics and store reports here (F8 captures a report)"),
        )
        .arg(
            flag("inspect-stdio")
                .requires("debug-dir")
                .help("Accept live inspector requests on stdin"),
        )
        .arg(
            path("record-report")
                .requires("headless")
                .conflicts_with("replay-from")
                .help("Write a replayable diagnostic report into a new directory"),
        )
        .arg(
            path("verify-report")
                .requires("headless")
                .conflicts_with_all([
                    "record-report",
                    "replay",
                    "ticks",
                    "arena",
                    "scenario",
                    "seed",
                ])
                .help("Verify a report's initial assets, input sequence and each completed tick"),
        );
    command
        .about("Research facility survivor. Move with WASD, choose an upgrade with 1/2/3.")
        .arg(
            Arg::new("scenario")
                .long("scenario")
                .value_parser(["early", "endgame"])
                .default_value("early"),
        )
        .arg(
            Arg::new("seed")
                .long("seed")
                .value_parser(value_parser!(u64))
                .default_value("506"),
        )
        .arg(
            Arg::new("skinning")
                .long("skinning")
                .value_parser(["cpu", "compute"])
                .conflicts_with("headless")
                .help("Character deformation path (default: compute with character shadows, cpu otherwise)"),
        )
        .arg(
            flag("character-shadows")
                .conflicts_with("headless")
                .help("Include moving characters in shadow maps"),
        )
        .arg(path("arena").help("Load edited arena JSON"))
        .arg(path("config").help("Engine settings JSON (default: optional sashiko.json beside the executable)"))
        .arg(flag("alternate-model").conflicts_with_all(["headless", "character"]))
        .arg(
            Arg::new("character")
                .long("character")
                .help("Character alias from the appearance catalog")
                .conflicts_with("headless"),
        )
        .arg(
            path("character-catalog")
                .help("Load a cooked character catalog")
                .conflicts_with("headless"),
        )
}

fn packaged_arena() -> std::io::Result<serde_json::Value> {
    let executable = std::env::current_exe()?;
    let root = executable
        .parent()
        .ok_or_else(|| std::io::Error::other("executable has no parent directory"))?;
    if root.join("sashiko-package.json").try_exists()? {
        let path = root.join("assets/survivor/arena.json");
        let data = fs::read_to_string(&path)
            .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        return Ok(serde_json::from_str(&data)?);
    }
    Ok(serde_json::from_str(include_str!(
        "../../../assets/survivor/arena.json"
    ))?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = arguments().get_matches();
    #[cfg(feature = "diagnostics")]
    if let Some(directory) = args.get_one::<PathBuf>("verify-report") {
        return diagnostics::verify(directory);
    }
    let engine = survivor::engine_config(args.get_one::<PathBuf>("config").map(PathBuf::as_path))?;
    let mut profile = profile::Player::load(&args)?;
    if args.get_flag("profile-status") {
        println!("{}", serde_json::to_string(&profile.prefs)?);
        return Ok(());
    }
    let explicit_arena = args
        .get_one::<PathBuf>("arena")
        .map(|p| -> std::io::Result<serde_json::Value> {
            Ok(serde_json::from_slice(&fs::read(p)?)?)
        })
        .transpose()?;
    let scenario = match args.get_one::<String>("scenario").map(String::as_str) {
        Some("endgame") => Scenario::Endgame,
        _ => Scenario::Early,
    };
    let seed = *args.get_one::<u64>("seed").unwrap();
    let requested = || -> std::io::Result<RunStart> {
        Ok(RunStart {
            arena: match &explicit_arena {
                Some(scene) => scene.clone(),
                None => packaged_arena()?,
            },
            scenario,
            seed,
            rules_version: 3,
            visual: None,
            locomotion: None,
        })
    };
    let (mut initial, mut replay) = if let Some(path) = args.get_one::<PathBuf>("replay") {
        let replay = Replay::<RunStart, Input>::read(fs::File::open(path)?, requested, rate())?;
        let init = replay.initial;
        if init.rules_version != 3 {
            return Err("replay uses an incompatible game rules version".into());
        }
        if explicit_arena
            .as_ref()
            .is_some_and(|scene| scene != &init.arena)
        {
            return Err("explicit arena conflicts with recorded replay".into());
        }
        for (arg, differs) in [
            ("seed", seed != init.seed),
            ("scenario", scenario != init.scenario),
        ] {
            if differs && args.value_source(arg) == Some(clap::parser::ValueSource::CommandLine) {
                return Err(format!("explicit {arg} conflicts with recorded replay").into());
            }
        }
        (init, Some(replay.inputs))
    } else {
        (requested()?, None)
    };
    let parallelism = engine.host_parallelism();
    let mut game = initial.game(sashiko::physics::PhysicsWorkers::new(parallelism)?)?;
    let explicit = args
        .get_one::<String>("character")
        .map(String::as_str)
        .or_else(|| args.get_flag("alternate-model").then_some("alternate"));
    let character = if let Some(visual) = &initial.visual {
        if explicit.is_some_and(|selected| selected != visual.character) {
            return Err("requested character conflicts with recorded appearance".into());
        }
        visual.character.clone()
    } else {
        explicit
            .unwrap_or_else(|| {
                if profile.prefs.alternate_suit_unlocked
                    && !["record", "replay", "frames"]
                        .iter()
                        .any(|key| args.contains_id(key))
                {
                    "alternate"
                } else {
                    "primary"
                }
            })
            .to_owned()
    };
    let catalog_path = args
        .get_one::<PathBuf>("character-catalog")
        .cloned()
        .map(Ok)
        .unwrap_or_else(survivor::catalog_path)?;
    #[cfg(feature = "diagnostics")]
    let offscreen = args.contains_id("record-report") && args.contains_id("backend");
    #[cfg(not(feature = "diagnostics"))]
    let offscreen = false;
    if initial.locomotion.is_none() && (!args.get_flag("headless") || offscreen) {
        let catalog = sashiko::animation::CharacterCatalog::open(&catalog_path)?;
        initial.locomotion = Some(survivor::LocomotionClips::load(&catalog, &character)?);
    }
    if let Some(clips) = &initial.locomotion {
        game.locomotion = clips.play()?;
    }
    if let Some(&prefix) = args.get_one::<usize>("replay-from") {
        let inputs = replay.as_mut().expect("replay required by CLI");
        if prefix >= inputs.len() {
            return Err(
                "replay prefix must leave at least one input for the requested segment".into(),
            );
        }
        for input in inputs.drain(..prefix) {
            profile.observe(game.step(input).kills)?;
        }
    }
    let game = if args.get_flag("headless") {
        let mut app = App::new(game, rate());
        let ticks = args
            .get_one::<usize>("ticks")
            .copied()
            .unwrap_or_else(|| replay.as_ref().map_or(600, Vec::len));
        if replay.as_ref().is_some_and(|inputs| ticks > inputs.len()) {
            return Err("replay ends before the requested tick count".into());
        }
        #[cfg(feature = "diagnostics")]
        let mut evidence = args
            .get_one::<PathBuf>("record-report")
            .map(|_| diagnostics::Evidence::new(&initial, parallelism, ticks))
            .transpose()?;
        app.try_advance(
            ticks as u128,
            |tick, game| {
                replay
                    .as_ref()
                    .map_or_else(|| benchmark_input(game), |inputs| inputs[tick])
            },
            |game, input| {
                let events = game.step(input);
                #[cfg(feature = "diagnostics")]
                if let Some(recorder) = &mut evidence {
                    recorder.observe(game, input, events);
                }
                profile.observe(events.kills)
            },
        )?;
        #[cfg(feature = "diagnostics")]
        if let Some(evidence) = evidence {
            let directory = args.get_one::<PathBuf>("record-report").unwrap();
            if offscreen {
                #[cfg(all(
                    windows,
                    feature = "presentation",
                    any(feature = "vulkan", feature = "d3d12")
                ))]
                {
                    let mut evidence = evidence;
                    let shadows = initial.visual.as_ref().is_some_and(|v| v.character_shadows);
                    let visual = survivor::visual::Visual::new(
                        &app.game,
                        &catalog_path,
                        &character,
                        survivor::visual::CharacterRendering {
                            shadows,
                            compute_skin: shadows,
                        },
                        sashiko::jobs::SharedJobs::from(sashiko::jobs::JobSystem::new(
                            parallelism,
                        )?),
                    )?;
                    evidence.set_visual(visual.content(&character))?;
                    let extent = [profile.prefs.width, profile.prefs.height];
                    let submitted = evidence.elapsed_ns()?;
                    let pixels =
                        window::render_capture(&app.game, extent, backend(&args)?, visual)?;
                    evidence.save(directory, "1", |m| {
                        m.settings["backend"] =
                            serde_json::json!(args.get_one::<String>("backend"));
                        diagnostics::save_image(
                            directory,
                            m,
                            sashiko::inspect::report::FrameStamp {
                                frame: 0,
                                tick: app.game.tick,
                                elapsed_ns: submitted,
                                interpolation: None,
                                view: "main".into(),
                                stage: "final_srgb_ui".into(),
                                extent,
                            },
                            pixels.rgba(),
                        )
                    })?;
                }
                #[cfg(not(all(
                    windows,
                    feature = "presentation",
                    any(feature = "vulkan", feature = "d3d12")
                )))]
                return Err("offscreen Survivor images require a Windows rendering build".into());
            } else {
                evidence.save(directory, "1", |_| Ok(()))?;
            }
        }
        app.game
    } else {
        play(
            game,
            initial,
            replay,
            &args,
            &mut profile,
            parallelism,
            (character, catalog_path),
        )?
    };
    #[cfg(feature = "diagnostics")]
    if args.get_flag("inspect-stdio") {
        return Ok(());
    }
    println!("{}", serde_json::to_string(&game.snapshot())?);
    Ok(())
}

#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
fn backend(
    args: &clap::ArgMatches,
) -> Result<sashiko_renderer::core::runtime::RuntimeConfig, Box<dyn Error>> {
    use sashiko_renderer::core::runtime::RuntimeConfig;
    let default_api = if cfg!(feature = "vulkan") {
        "vulkan"
    } else {
        "d3d12"
    };
    Ok(
        match args
            .get_one::<String>("backend")
            .map_or(default_api, String::as_str)
        {
            #[cfg(feature = "vulkan")]
            "vulkan" => RuntimeConfig::Vulkan(Default::default()),
            #[cfg(feature = "d3d12")]
            "d3d12" => RuntimeConfig::D3D12(Default::default()),
            _ => return Err("selected backend is unavailable in this build".into()),
        },
    )
}

#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
fn play(
    game: Game,
    initial: RunStart,
    replay: Option<Vec<Input>>,
    args: &clap::ArgMatches,
    profile: &mut profile::Player,
    parallelism: std::num::NonZeroUsize,
    (character, catalog_path): (String, PathBuf),
) -> Result<Game, Box<dyn Error>> {
    let config = backend(args)?;
    let requested_shadows = args.get_flag("character-shadows");
    let shadows = if let Some(visual) = &initial.visual {
        if requested_shadows && !visual.character_shadows {
            return Err("requested character shadows conflict with recorded appearance".into());
        }
        visual.character_shadows
    } else {
        requested_shadows
    };
    window::run(
        game,
        initial,
        window::Options {
            #[cfg(feature = "diagnostics")]
            debug_dir: args.get_one::<PathBuf>("debug-dir").cloned(),
            #[cfg(feature = "diagnostics")]
            inspect_stdio: args.get_flag("inspect-stdio"),
            config,
            parallelism,
            replay,
            extent: [profile.prefs.width, profile.prefs.height],
            volume: profile.prefs.volume,
            frames: args.get_one("frames").copied(),
            warmup: args.get_one("warmup").copied().unwrap_or(0),
            record: args.get_one("record").cloned(),
            metrics: args.get_one("metrics").cloned(),
            memory: args.get_one("memory-samples").cloned(),
            capture: args.get_one("capture").cloned(),
            gpu_profile: args.get_flag("gpu-profile"),
            character,
            characters: survivor::visual::CharacterRendering {
                compute_skin: args
                    .get_one::<String>("skinning")
                    .map_or(shadows, |v| v == "compute"),
                shadows,
            },
            catalog: catalog_path,
        },
        profile,
    )
}

#[cfg(not(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
)))]
fn play(
    _: Game,
    _: RunStart,
    _: Option<Vec<Input>>,
    _: &clap::ArgMatches,
    _: &mut profile::Player,
    _: std::num::NonZeroUsize,
    _: (String, PathBuf),
) -> Result<Game, Box<dyn Error>> {
    Err("use --headless or enable presentation and a graphics backend on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_retains_the_authored_rule_parameters() {
        let mut arena: serde_json::Value =
            serde_json::from_str(include_str!("../../../assets/survivor/arena.json")).unwrap();
        arena["prefabs"]["goal"]["fields"]["radius"] = serde_json::json!(4.);
        let start = RunStart {
            arena,
            seed: 506,
            scenario: Scenario::Early,
            rules_version: 3,
            visual: None,
            locomotion: None,
        };
        let tape = Replay::new(start, rate(), vec![Input::default()]);
        let wire = serde_json::to_vec(&tape).unwrap();
        let loaded = Replay::<RunStart, Input>::read(
            &wire[..],
            || panic!("recorded scene should be sufficient"),
            rate(),
        )
        .unwrap();
        let mut game = loaded.initial.game(Default::default()).unwrap();
        assert!(game.step(loaded.inputs[0]).reached_marker);
    }
}

use clap::{Arg, ArgAction, Command, value_parser};
use std::{error::Error, fs, path::PathBuf};
use survivor::{Arena, Game, Input, Scenario, benchmark_input};

#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
mod character;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
mod visual;
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
    Command::new("survivor")
        .about("Research facility survivor. Move with WASD, choose an upgrade with 1/2/3.")
        .arg(
            flag("headless")
                .help("Run fixed simulation ticks without a GPU")
                .conflicts_with("window"),
        )
        .arg(flag("window").help("Play or measure the Windows game"))
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
        .arg(path("arena").help("Load edited arena JSON"))
        .arg(path("replay").help("Consume one recorded input per tick"))
        .arg(
            path("record")
                .help("Write window inputs as a JSON array")
                .conflicts_with("headless"),
        )
        .arg(
            Arg::new("backend")
                .long("backend")
                .value_parser(["vulkan", "d3d12"]),
        )
        .arg(
            Arg::new("ticks")
                .long("ticks")
                .value_parser(value_parser!(usize))
                .requires("headless"),
        )
        .arg(
            Arg::new("frames")
                .long("frames")
                .value_parser(value_parser!(usize))
                .conflicts_with("headless")
                .help("Number of successfully presented frames after warmup"),
        )
        .arg(
            Arg::new("warmup")
                .long("warmup")
                .value_parser(value_parser!(usize))
                .requires("frames"),
        )
        .arg(
            Arg::new("width")
                .long("width")
                .value_parser(value_parser!(u32).range(1..))
                .default_value("1920"),
        )
        .arg(
            Arg::new("height")
                .long("height")
                .value_parser(value_parser!(u32).range(1..))
                .default_value("1080"),
        )
        .arg(
            path("metrics")
                .requires("frames")
                .help("Write per-frame CSV"),
        )
        .arg(
            path("capture")
                .requires("frames")
                .help("Write a PPM after measurement"),
        )
        .arg(flag("gpu-profile").requires("metrics"))
        .arg(flag("alternate-model").conflicts_with("headless"))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = arguments().get_matches();
    let arena = match args.get_one::<PathBuf>("arena") {
        Some(path) => Arena::from_json(&fs::read_to_string(path)?)?,
        None => Arena::default(),
    };
    let scenario = match args.get_one::<String>("scenario").map(String::as_str) {
        Some("endgame") => Scenario::Endgame,
        _ => Scenario::Early,
    };
    let seed = *args.get_one::<u64>("seed").unwrap();
    let game = Game::new(arena, scenario, seed);
    let replay: Option<Vec<Input>> = args
        .get_one::<PathBuf>("replay")
        .map(|p| -> Result<_, Box<dyn Error>> { Ok(serde_json::from_slice(&fs::read(p)?)?) })
        .transpose()?;
    let game = if args.get_flag("headless") {
        let mut game = game;
        let ticks = args
            .get_one::<usize>("ticks")
            .copied()
            .unwrap_or_else(|| replay.as_ref().map_or(600, Vec::len));
        if replay.as_ref().is_some_and(|inputs| ticks > inputs.len()) {
            return Err("replay ends before the requested tick count".into());
        }
        for tick in 0..ticks {
            let input = replay
                .as_ref()
                .map_or_else(|| benchmark_input(&game), |inputs| inputs[tick]);
            game.step(input);
        }
        game
    } else {
        play(game, replay, &args)?
    };
    println!("{}", serde_json::to_string(&game.snapshot())?);
    Ok(())
}

#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
fn play(
    game: Game,
    replay: Option<Vec<Input>>,
    args: &clap::ArgMatches,
) -> Result<Game, Box<dyn Error>> {
    use renderer::core::runtime::RuntimeConfig;
    let default_api = if cfg!(feature = "vulkan") {
        "vulkan"
    } else {
        "d3d12"
    };
    let config = match args
        .get_one::<String>("backend")
        .map_or(default_api, String::as_str)
    {
        #[cfg(feature = "vulkan")]
        "vulkan" => RuntimeConfig::Vulkan(Default::default()),
        #[cfg(feature = "d3d12")]
        "d3d12" => RuntimeConfig::D3D12(Default::default()),
        _ => return Err("selected backend is unavailable in this build".into()),
    };
    window::run(
        game,
        window::Options {
            config,
            replay,
            extent: [
                *args.get_one("width").unwrap(),
                *args.get_one("height").unwrap(),
            ],
            frames: args.get_one("frames").copied(),
            warmup: args.get_one("warmup").copied().unwrap_or(0),
            record: args.get_one("record").cloned(),
            metrics: args.get_one("metrics").cloned(),
            capture: args.get_one("capture").cloned(),
            gpu_profile: args.get_flag("gpu-profile"),
            alternate_model: args.get_flag("alternate-model"),
        },
    )
}

#[cfg(not(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
)))]
fn play(_: Game, _: Option<Vec<Input>>, _: &clap::ArgMatches) -> Result<Game, Box<dyn Error>> {
    Err("use --headless or enable presentation and a graphics backend on Windows".into())
}
